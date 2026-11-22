import { afterAll, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import { PRESETS, themeJson } from './theme';
import { readBook, resolve } from './theme-library';

// The live copy of the user's themes: what is on the page, what is written, and what an editing sitting does and does
// not leave behind. The page root, the dialogs and the toasts are stood in for.

const g = globalThis as unknown as { document?: unknown; window?: unknown };
const original = { document: g.document, window: g.window };

const props: Record<string, string> = {};
const classes: Record<string, boolean> = {};
g.document = {
	documentElement: {
		style: { setProperty: (k: string, v: string) => void (props[k] = v), colorScheme: '', background: '' },
		classList: { toggle: (k: string, on: boolean) => void (classes[k] = on) }
	}
};

const listeners: ((e: { matches: boolean }) => void)[] = [];
let systemDark = false;
g.window = {
	location: { search: '' },
	matchMedia: () => ({ matches: systemDark, addEventListener: (_: string, cb: (e: { matches: boolean }) => void) => listeners.push(cb) })
};
const flip = (dark: boolean) => {
	systemDark = dark;
	for (const l of listeners) l({ matches: dark });
};

const real = { ...(await import('./api')) };
let picked: string | null = null;
let answer = true;
const asked: string[] = [];
const saved: { contents: string; name: string }[] = [];
mock.module('./api', () => ({
	...real,
	importThemeFile: async () => picked,
	exportThemeFile: async (contents: string, name: string) => {
		saved.push({ contents, name });
		return `/tmp/${name}`;
	},
	confirmAction: async (message: string) => {
		asked.push(message);
		return answer;
	}
}));
const toasts: { kind: string; title: string; description?: string }[] = [];
const note = (kind: string) => (title: string, o?: { description?: string }) => void toasts.push({ kind, title, description: o?.description });
mock.module('./notifications.svelte', () => ({ toast: Object.assign(() => {}, { success: note('success'), info: note('info'), error: note('error'), warning: note('warning'), dismiss() {} }) }));
afterAll(() => {
	mock.module('./api', () => real);
	for (const k of ['document', 'window'] as const) {
		if (original[k] === undefined) delete g[k];
		else g[k] = original[k];
	}
});

const { ThemeBookState } = await import('./theme-book.svelte');

/** A state with a store that records what it is asked to write, and can be held up. */
function make(raw: unknown = null, hold = false) {
	const written: unknown[] = [];
	const gates: (() => void)[] = [];
	const state = new ThemeBookState(async (blob) => {
		written.push(JSON.parse(JSON.stringify(blob)));
		if (hold) await new Promise<void>((r) => gates.push(r));
	});
	state.read(raw);
	return { state, written, gates };
}
const settle = () => new Promise((r) => setTimeout(r, 0));

describe('reading and applying', () => {
	test('nothing stored is Kerf Dark, applied to the page, and nothing is written for it', () => {
		const { state, written } = make();
		expect(state.current).toBe(PRESETS['kerf-dark']);
		expect(props['--kerf-500']).toBe(PRESETS['kerf-dark'].colors['kerf-500']);
		expect(classes.dark).toBe(true);
		expect(written).toEqual([]);
	});

	test('a theme stored by 0.31 is in use as it was, and still nothing is written', () => {
		const custom = { ...PRESETS['kerf-light'], name: 'Mine', colors: { ...PRESETS['kerf-light'].colors, 'kerf-500': '#123456' } };
		const { state, written } = make(JSON.parse(themeJson(custom)));
		expect(state.current.name).toBe('Mine');
		expect(props['--kerf-500']).toBe('#123456');
		expect(classes.dark).toBe(false);
		expect(written).toEqual([]);
	});

	test('a pick is in use at once and written once, as a book that also reads as the flat theme', () => {
		const { state, written } = make();
		state.select('kerf-light');
		expect(state.current).toBe(PRESETS['kerf-light']);
		expect(props['--surface-app']).toBe(PRESETS['kerf-light'].colors['surface-app']);
		expect(written.length).toBe(1);
		const blob = written[0] as Record<string, unknown>;
		expect(blob.name).toBe('Kerf Light');
		expect((blob.library as { active: string }).active).toBe('kerf-light');
		expect(state.inForceId).toBe('kerf-light');
	});

	test('picking what is in use writes nothing', () => {
		const { state, written } = make();
		state.select('kerf-dark');
		expect(written).toEqual([]);
	});
});

describe('writing', () => {
	test('one write at a time, newest wins: changes made while one is in flight make one follow-up with the latest book', async () => {
		const { state, written, gates } = make(null, true);
		state.select('kerf-light');
		state.select('high-contrast');
		state.select('kerf-dark');
		state.select('kerf-light');
		expect(written.length).toBe(1);
		gates.shift()!();
		await settle();
		expect(written.length).toBe(2);
		expect((written[1] as { library: { active: string } }).library.active).toBe('kerf-light');
		gates.shift()!();
		await settle();
		expect(written.length).toBe(2);
	});
});

describe('the library', () => {
	test('a copy is the user\'s own and joins the list, not in use', () => {
		const { state, written } = make();
		const id = state.duplicate('high-contrast')!;
		expect(state.entries.map((e) => e.id)).toContain(id);
		expect(state.entries.find((e) => e.id === id)!.theme.name).toBe('High contrast copy');
		expect(state.inForceId).toBe('kerf-dark');
		expect(written.length).toBe(1);
	});

	test('rename says why it cannot, and does it when it can', () => {
		const { state } = make();
		const id = state.duplicate('kerf-dark')!;
		const bad = state.rename(id, 'Kerf Light');
		expect(bad.ok).toBe(false);
		expect(state.rename(id, 'Night').ok).toBe(true);
		expect(state.entries.find((e) => e.id === id)!.theme.name).toBe('Night');
		expect(state.rename('kerf-dark', 'x').ok).toBe(false);
	});

	test('delete asks, and does nothing if the answer is no', async () => {
		const { state } = make();
		const id = state.duplicate('kerf-dark')!;
		asked.length = 0;
		answer = false;
		expect(await state.remove(id)).toBe(false);
		expect(state.entries.some((e) => e.id === id)).toBe(true);
		answer = true;
		expect(await state.remove(id)).toBe(true);
		expect(state.entries.some((e) => e.id === id)).toBe(false);
		expect(asked.length).toBe(2);
		expect(await state.remove('kerf-dark')).toBe(false);
		expect(asked.length).toBe(2);
	});

	test('deleting the theme in use falls back to the built-in of its scheme', async () => {
		const { state } = make();
		const id = state.duplicate('kerf-light')!;
		state.select(id);
		answer = true;
		await state.remove(id);
		expect(state.inForceId).toBe('kerf-light');
		expect(state.current).toBe(PRESETS['kerf-light']);
	});
});

describe('files', () => {
	test('an export is the theme as JSON under a file name made from its name', async () => {
		const { state } = make();
		const id = state.duplicate('kerf-dark')!;
		state.rename(id, 'My night!');
		saved.length = 0;
		toasts.length = 0;
		await state.exportFile(id);
		expect(saved[0].name).toBe('my-night.kerf-theme.json');
		const file = JSON.parse(saved[0].contents);
		expect(file.name).toBe('My night!');
		expect('id' in file).toBe(false);
		expect(file.colors).toEqual(PRESETS['kerf-dark'].colors);
		expect(toasts.at(-1)!.kind).toBe('success');
	});

	test('an import is added under a free name and used; the old one is still in the list', async () => {
		const { state, written } = make();
		picked = themeJson({ ...PRESETS['high-contrast'], name: 'Kerf Dark', colors: { ...PRESETS['high-contrast'].colors, waveform: '#123456' } });
		toasts.length = 0;
		expect(await state.importFile()).toBe(true);
		const mine = state.entries.filter((e) => !e.builtIn);
		expect(mine.map((e) => e.theme.name)).toEqual(['Kerf Dark 2']);
		expect(state.inForceId).toBe(mine[0].id);
		expect(props['--waveform']).toBe('#123456');
		expect(written.length).toBe(1);
		expect(toasts.at(-1)!.kind).toBe('success');
	});

	test('what is not a theme is refused with a reason, and nothing changes', async () => {
		const { state, written } = make();
		for (const text of ['{nope', '[]', JSON.stringify({ ...PRESETS['kerf-dark'], colors: { ...PRESETS['kerf-dark'].colors, waveform: 'red' } }), 'x'.repeat(300_000)]) {
			picked = text;
			toasts.length = 0;
			expect(await state.importFile()).toBe(false);
			expect(toasts.at(-1)!.kind).toBe('error');
			expect(toasts.at(-1)!.description!.length).toBeGreaterThan(5);
		}
		expect(state.entries.every((e) => e.builtIn)).toBe(true);
		expect(written).toEqual([]);
		expect(state.inForceId).toBe('kerf-dark');
	});

	test('a cancelled picker does nothing', async () => {
		const { state } = make();
		picked = null;
		toasts.length = 0;
		expect(await state.importFile()).toBe(false);
		expect(toasts).toEqual([]);
	});

	test('at the limit an import says so', async () => {
		const { state } = make();
		for (let i = 0; i < 40; i++) state.duplicate('kerf-dark');
		picked = themeJson(PRESETS['kerf-light']);
		toasts.length = 0;
		expect(await state.importFile()).toBe(false);
		expect(toasts.at(-1)!.kind).toBe('error');
		expect(state.canAdd).toBe(false);
	});
});

describe('the editor', () => {
	test('opening it on a built-in changes nothing in the library; cancelling leaves nothing behind', async () => {
		const { state, written } = make();
		expect(state.edit('kerf-dark')).toBe(true);
		expect(state.entries.every((e) => e.builtIn)).toBe(true);
		expect(state.editing!.draft.name).toBe('Kerf Dark copy');
		state.setColor('kerf-500', '#ff00aa');
		expect(props['--kerf-500']).toBe('#ff00aa');
		expect(state.current.colors['kerf-500']).toBe('#ff00aa');
		expect(state.dirty).toBe(true);
		answer = true;
		expect(await state.close()).toBe(true);
		expect(state.editing).toBeNull();
		expect(props['--kerf-500']).toBe(PRESETS['kerf-dark'].colors['kerf-500']);
		expect(state.entries.every((e) => e.builtIn)).toBe(true);
		expect(state.inForceId).toBe('kerf-dark');
		expect(written).toEqual([]);
	});

	test('saving a copy of a built-in adds it, uses it and writes it once', () => {
		const { state, written } = make();
		state.edit('kerf-dark');
		state.setColor('kerf-500', '#ff00aa');
		state.setChoice('density', 'compact');
		state.setName('Pink');
		expect(state.save()).toBe(true);
		expect(state.editing).toBeNull();
		const mine = state.entries.filter((e) => !e.builtIn);
		expect(mine.map((e) => e.theme.name)).toEqual(['Pink']);
		expect(state.inForceId).toBe(mine[0].id);
		expect(props['--control-sm']).toBe('24px');
		expect(written.length).toBe(1);
		// What was written reads back as the same book.
		const back = readBook(written[0]);
		expect(back.themes[0].colors['kerf-500']).toBe('#ff00aa');
		expect(back.themes[0].base).toBe('kerf-dark');
		expect(resolve(back, false).id).toBe(mine[0].id);
	});

	test('a sitting on a theme of the user\'s: edits show at once, Save keeps them, Cancel does not', async () => {
		const { state, written } = make();
		const id = state.duplicate('kerf-dark')!;
		written.length = 0;
		state.edit(id);
		state.setShape('line-width', 2);
		expect(props['--line-width']).toBe('2px');
		expect(state.book.themes[0].shape['line-width']).toBe(1);
		answer = true;
		await state.close();
		expect(props['--line-width']).toBe('1px');
		state.edit(id);
		state.setShape('line-width', 2);
		state.save();
		expect(state.book.themes[0].shape['line-width']).toBe(2);
		expect(written.length).toBe(1);
	});

	test('closing with nothing changed does not ask; with something changed it does, and no keeps the sitting', async () => {
		const { state } = make();
		state.edit('kerf-dark');
		asked.length = 0;
		expect(await state.close()).toBe(true);
		expect(asked).toEqual([]);
		state.edit('kerf-dark');
		state.setColor('kerf-500', '#ff00aa');
		answer = false;
		expect(await state.close()).toBe(false);
		expect(asked.length).toBe(1);
		expect(state.editing).not.toBeNull();
		expect(state.current.colors['kerf-500']).toBe('#ff00aa');
		answer = true;
		expect(await state.close()).toBe(true);
	});

	test('saving without a name says so and stays open; saving with nothing changed writes nothing', () => {
		const { state, written } = make();
		const id = state.duplicate('kerf-dark')!;
		written.length = 0;
		state.edit(id);
		toasts.length = 0;
		expect(state.save()).toBe(true);
		expect(written).toEqual([]);
		state.edit(id);
		state.setName('   ');
		expect(state.save()).toBe(false);
		expect(toasts.at(-1)!.kind).toBe('error');
		expect(state.editing).not.toBeNull();
		state.setName('Fine');
		expect(state.save()).toBe(true);
	});

	test('undo and redo walk the draft, and what is on the page follows', () => {
		const { state } = make();
		state.edit('kerf-dark');
		state.setColor('kerf-500', '#111111');
		state.endEdit();
		state.setColor('kerf-500', '#222222');
		state.undo();
		expect(props['--kerf-500']).toBe('#111111');
		state.undo();
		expect(props['--kerf-500']).toBe(PRESETS['kerf-dark'].colors['kerf-500']);
		state.redo();
		expect(props['--kerf-500']).toBe('#111111');
		state.revertAll();
		expect(props['--kerf-500']).toBe(PRESETS['kerf-dark'].colors['kerf-500']);
	});

	test('a second sitting cannot open over the first', () => {
		const { state } = make();
		expect(state.edit('kerf-dark')).toBe(true);
		expect(state.edit('kerf-light')).toBe(false);
	});
});

describe('following the system', () => {
	test('the theme changes when the system does, and a draft is not disturbed', () => {
		systemDark = false;
		listeners.length = 0;
		const { state, written } = make();
		state.setFollow({ on: true });
		written.length = 0;
		expect(state.inForceId).toBe('kerf-light');
		expect(props['--surface-app']).toBe(PRESETS['kerf-light'].colors['surface-app']);
		flip(true);
		expect(state.inForceId).toBe('kerf-dark');
		expect(props['--surface-app']).toBe(PRESETS['kerf-dark'].colors['surface-app']);
		state.edit('kerf-dark');
		state.setColor('surface-app', '#123456');
		flip(false);
		expect(props['--surface-app']).toBe('#123456');
		state.cancel();
		expect(props['--surface-app']).toBe(PRESETS['kerf-light'].colors['surface-app']);
	});

	test('it starts in the scheme the system is in', () => {
		systemDark = true;
		listeners.length = 0;
		const { state } = make({ ...JSON.parse(themeJson(PRESETS['kerf-dark'])), library: { version: 1, follow: { on: true } } });
		expect(state.inForceId).toBe('kerf-dark');
		systemDark = false;
	});

	test('what is stored while following also reads as the theme in force, for a build that knows no books', async () => {
		systemDark = true;
		listeners.length = 0;
		const { state, written } = make();
		state.setFollow({ on: true });
		await settle();
		expect((written.at(-1) as { name: string }).name).toBe('Kerf Dark');
		flip(false);
		state.setFollow({ light: 'high-contrast' });
		state.setFollow({ dark: 'high-contrast' });
		await settle();
		const last = written.at(-1) as { name: string; library: { follow: { on: boolean; dark: string; light: string } } };
		expect(last.name).toBe('Kerf Light');
		expect(last.library.follow).toEqual({ on: true, light: 'kerf-light', dark: 'high-contrast' });
		systemDark = false;
	});
});

describe('starting over', () => {
	test('forgetting goes back to Kerf Dark, ready to read the fresh view', () => {
		const { state } = make();
		state.select('kerf-light');
		state.edit('kerf-light');
		state.forget();
		expect(state.editing).toBeNull();
		expect(state.inForceId).toBe('kerf-dark');
		expect(props['--surface-app']).toBe(PRESETS['kerf-dark'].colors['surface-app']);
	});
});
