import { describe, expect, test } from 'bun:test';
import { PRESETS, PRESET_IDS, SUPERSEDED_KERF_LIGHT, parseTheme, themeFields, themeJson, type Theme } from './theme';
import {
	MAX_THEMES,
	add,
	bookBlob,
	copyName,
	duplicate,
	emptyBook,
	entries,
	entriesFor,
	isBuiltIn,
	lookup,
	newId,
	readBook,
	LIBRARY_VERSION,
	LIMIT_REASON,
	placement,
	placementNote,
	readStored,
	remove,
	rename,
	resolve,
	select,
	setFollow,
	uniqueName,
	update,
	type ThemeBook,
	type UserTheme
} from './theme-library';

/** A counter in place of `Math.random`, so ids are known. */
const counter = () => {
	let n = 0;
	return () => (++n * 1234567) / 36 ** 8;
};

const mine = (book: ThemeBook, from = 'kerf-dark') => {
	const out = duplicate(book, from, counter());
	if (!out.ok) throw new Error(out.reason);
	return out;
};

describe('the book', () => {
	test('an empty one has the three built-ins, Kerf Dark in use, and no system-follow', () => {
		const book = emptyBook();
		expect(entries(book).map((e) => e.id)).toEqual(PRESET_IDS);
		expect(entries(book).every((e) => e.builtIn)).toBe(true);
		expect(resolve(book, true).id).toBe('kerf-dark');
		expect(resolve(book, false).id).toBe('kerf-dark');
		expect(book.follow.on).toBe(false);
	});

	test('the built-ins come first, then the user\'s by name as a person reads it; a scheme lists its own', () => {
		const a = mine(emptyBook());
		const b = mine(a.book, 'kerf-light');
		expect(entries(b.book).map((e) => e.id)).toEqual([...PRESET_IDS, a.id, b.id].sort((x, y) => (PRESET_IDS.includes(x as never) ? 0 : 1) - (PRESET_IDS.includes(y as never) ? 0 : 1) || 0));
		expect(entriesFor(b.book, 'light').map((e) => e.id)).toEqual(['kerf-light', b.id]);
		expect(entriesFor(b.book, 'dark').map((e) => e.id)).toEqual(['kerf-dark', 'high-contrast', a.id]);
	});

	test('a theme that has gone, or was never there, is Kerf Dark', () => {
		const book: ThemeBook = { ...emptyBook(), active: 'u-nothing' };
		expect(resolve(book, false)).toEqual({ id: 'kerf-dark', theme: PRESETS['kerf-dark'] });
		expect(lookup(book, 'u-nothing')).toBeNull();
		expect(isBuiltIn('kerf-light')).toBe(true);
		expect(isBuiltIn('u-x')).toBe(false);
	});
});

describe('switching', () => {
	test('a pick is in use at once; an unknown one changes nothing', () => {
		const book = emptyBook();
		expect(select(book, 'kerf-light').active).toBe('kerf-light');
		expect(select(book, 'u-gone')).toBe(book);
		expect(select(book, 'kerf-dark')).toBe(book);
	});

	test('following the system, a pick is assigned to its own scheme\'s slot', () => {
		const book = setFollow(emptyBook(), { on: true });
		const out = select(book, 'high-contrast');
		expect(out.follow.dark).toBe('high-contrast');
		expect(out.follow.light).toBe('kerf-light');
		expect(out.active).toBe(book.active);
		expect(select(out, 'kerf-light')).toBe(out);
	});
});

describe('following the system', () => {
	test('the slot of the system\'s scheme is in use', () => {
		let book = setFollow(emptyBook(), { on: true });
		expect(resolve(book, true).id).toBe('kerf-dark');
		expect(resolve(book, false).id).toBe('kerf-light');
		const m = mine(book, 'high-contrast');
		book = setFollow(m.book, { dark: m.id });
		expect(resolve(book, true).id).toBe(m.id);
		expect(resolve(book, false).id).toBe('kerf-light');
	});

	test('a slot only takes a theme of its scheme, and one that exists', () => {
		const book = setFollow(emptyBook(), { on: true });
		expect(setFollow(book, { light: 'kerf-dark' })).toBe(book);
		expect(setFollow(book, { dark: 'kerf-light' })).toBe(book);
		expect(setFollow(book, { dark: 'u-gone' })).toBe(book);
		expect(setFollow(book, { light: 'kerf-light', dark: 'kerf-dark' })).toBe(book);
	});

	test('turning it on keeps the theme in use in its slot', () => {
		const m = mine(select(emptyBook(), 'kerf-light'), 'kerf-light');
		const on = setFollow(select(m.book, m.id), { on: true });
		expect(on.follow.light).toBe(m.id);
		expect(on.follow.dark).toBe('kerf-dark');
		expect(resolve(on, false).id).toBe(m.id);
		const off = setFollow(on, { on: false });
		expect(resolve(off, false).id).toBe(m.id);
	});
});

describe('copies', () => {
	test('a copy of a built-in is the user\'s own, remembers where it came from and shares nothing', () => {
		const out = mine(emptyBook(), 'high-contrast');
		const copy = out.book.themes[0];
		expect(copy.name).toBe('High contrast copy');
		expect(copy.base).toBe('high-contrast');
		expect(copy.colors).toEqual(PRESETS['high-contrast'].colors);
		expect(copy.colors).not.toBe(PRESETS['high-contrast'].colors);
		expect(copy.shape).not.toBe(PRESETS['high-contrast'].shape);
		expect(out.id).toMatch(/^u-[a-z0-9]{8}$/);
		expect(isBuiltIn(out.id)).toBe(false);
		expect(out.book.active).toBe('kerf-dark');
	});

	test('a copy of a copy keeps the built-in it was made from', () => {
		const a = mine(emptyBook(), 'kerf-light');
		const b = mine(a.book, a.id);
		expect(b.book.themes[1].base).toBe('kerf-light');
		expect(b.book.themes[1].name).toBe('Kerf Light copy 2');
	});

	test('names are free: copy, copy 2, copy 3, whatever the case or spacing', () => {
		expect(copyName('Kerf Dark', ['Kerf Dark'])).toBe('Kerf Dark copy');
		expect(copyName('Kerf Dark', ['Kerf Dark', 'kerf  dark COPY'])).toBe('Kerf Dark copy 2');
		expect(copyName('Kerf Dark copy', ['Kerf Dark', 'Kerf Dark copy'])).toBe('Kerf Dark copy 2');
		expect(copyName('Kerf Dark copy 7', ['Kerf Dark', 'Kerf Dark copy'])).toBe('Kerf Dark copy 2');
		expect(copyName('copy', [])).toBe('copy copy');
		expect(uniqueName('Mine', ['mine', 'Mine 2'])).toBe('Mine 3');
		expect(uniqueName('  ', [])).toBe('Theme');
		expect(uniqueName('x'.repeat(60), ['x'.repeat(48)]).length).toBeLessThanOrEqual(48);
		expect(uniqueName('x'.repeat(60), ['x'.repeat(48)])).toMatch(/ 2$/);
	});

	test('ids are never one already used', () => {
		let calls = 0;
		const id = newId(['u-00000000'], () => (calls++ === 0 ? 0 : 0.5));
		expect(id).not.toBe('u-00000000');
		expect(calls).toBe(2);
		expect(newId([], () => 0.5)).toMatch(/^u-[a-z0-9]{8}$/);
	});

	test('there is a limit, and a copy past it says so instead of doing something near it', () => {
		let book = emptyBook();
		for (let i = 0; i < MAX_THEMES; i++) book = mine(book).book;
		const over = duplicate(book, 'kerf-dark');
		expect(over.ok).toBe(false);
		expect(!over.ok && over.reason).toContain(String(MAX_THEMES));
		expect(add(book, PRESETS['kerf-dark']).ok).toBe(false);
		expect(duplicate(emptyBook(), 'u-gone').ok).toBe(false);
	});
});

describe('imports', () => {
	test('a theme that came from a file is added under a free name, as it is', () => {
		const file = { ...PRESETS['kerf-dark'], name: 'Kerf Dark' } as Theme;
		const out = add(emptyBook(), file, counter());
		expect(out.ok).toBe(true);
		if (!out.ok) return;
		expect(out.book.themes[0].name).toBe('Kerf Dark 2');
		expect(out.book.themes[0].colors).toEqual(file.colors);
		expect(out.book.active).toBe('kerf-dark');
		const again = add(out.book, file);
		expect(again.ok && again.book.themes.map((t) => t.name)).toEqual(['Kerf Dark 2', 'Kerf Dark 3']);
	});

	test('an old Kerf Light that is imported stays the old Kerf Light: only what the settings hold is upgraded', () => {
		const old = parseTheme({ ...PRESETS['kerf-light'], name: 'Old light', colors: SUPERSEDED_KERF_LIGHT[0] })!;
		const out = add(emptyBook(), old);
		expect(out.ok && out.book.themes[0].colors).toEqual(old.colors);
		expect(old.colors['kerf-500']).not.toBe(PRESETS['kerf-light'].colors['kerf-500']);
	});
});

describe('renaming, updating and deleting', () => {
	test('a built-in cannot be renamed, changed or deleted', () => {
		const book = emptyBook();
		expect(rename(book, 'kerf-dark', 'x').ok).toBe(false);
		expect(update(book, 'kerf-dark', PRESETS['kerf-dark']).ok).toBe(false);
		expect(remove(book, 'kerf-dark').ok).toBe(false);
	});

	test('a rename is cleaned, needs a name and may not take another theme\'s', () => {
		const a = mine(emptyBook());
		const ok = rename(a.book, a.id, '  Night   shift ');
		expect(ok.ok && ok.book.themes[0].name).toBe('Night shift');
		expect(rename(a.book, a.id, '   ').ok).toBe(false);
		const taken = rename(a.book, a.id, 'KERF LIGHT');
		expect(!taken.ok && taken.reason).toContain('already');
		const same = rename(a.book, a.id, a.book.themes[0].name);
		expect(same.ok && same.book).toBe(a.book);
		expect(rename(a.book, 'u-gone', 'x').ok).toBe(false);
	});

	test('saving an edit replaces the content and keeps the id; the name is held to the same rules', () => {
		const a = mine(emptyBook());
		const edited: Theme = { ...a.book.themes[0], name: 'Edited', colors: { ...a.book.themes[0].colors, waveform: '#123456' } };
		const out = update(a.book, a.id, edited);
		expect(out.ok && out.book.themes[0].id).toBe(a.id);
		expect(out.ok && out.book.themes[0].colors.waveform).toBe('#123456');
		expect(update(a.book, a.id, { ...edited, name: 'Kerf Dark' }).ok).toBe(false);
		expect(update(a.book, 'u-gone', edited).ok).toBe(false);
	});

	test('deleting what is in use puts the built-in of its scheme in its place, and its slots back', () => {
		const light = mine(emptyBook(), 'kerf-light');
		let book = select(light.book, light.id);
		book = setFollow(book, { on: true });
		expect(book.follow.light).toBe(light.id);
		const out = remove(book, light.id);
		expect(out.ok && out.book.active).toBe('kerf-light');
		expect(out.ok && out.book.follow.light).toBe('kerf-light');
		expect(out.ok && out.book.themes).toEqual([]);
		const dark = mine(emptyBook());
		const gone = remove(select(dark.book, dark.id), dark.id);
		expect(gone.ok && gone.book.active).toBe('kerf-dark');
		expect(remove(dark.book, 'u-gone').ok).toBe(false);
	});

	test('deleting another theme leaves the one in use alone', () => {
		const a = mine(emptyBook());
		const b = mine(a.book);
		const out = remove(select(b.book, a.id), b.id);
		expect(out.ok && out.book.active).toBe(a.id);
	});
});

describe('what is stored', () => {
	const ids = counter();
	const withThemes = () => {
		let book = emptyBook();
		const a = duplicate(book, 'kerf-dark', ids);
		if (!a.ok) throw new Error();
		const b = duplicate(a.book, 'kerf-light', ids);
		if (!b.ok) throw new Error();
		book = setFollow(select(b.book, a.id), { on: true, light: b.id });
		return { book, a: a.id, b: b.id };
	};

	test('a book reads back as itself, through JSON', () => {
		const { book } = withThemes();
		const blob = JSON.parse(JSON.stringify(bookBlob(book, resolve(book, true).theme)));
		expect(readBook(blob)).toEqual(book);
	});

	test('what is stored is also, on its own, the theme in force: a build that knows no books still reads it', () => {
		const { book, a } = withThemes();
		const dark = resolve(book, true);
		expect(dark.id).toBe(a);
		const blob = JSON.parse(JSON.stringify(bookBlob(book, dark.theme)));
		expect(parseTheme(blob)).toEqual(themeFields(dark.theme));
		expect(Object.keys(blob).sort()).toEqual(['base', 'colors', 'library', 'name', 'scheme', 'shape', 'version']);
		expect(blob.library.themes[0].id).toBe(a);
	});

	test('a theme stored by 0.31 is the active theme: a built-in as itself, a mix of the user\'s as theirs', () => {
		for (const id of PRESET_IDS) {
			const book = readBook(JSON.parse(themeJson(PRESETS[id])));
			expect(book.active).toBe(id);
			expect(book.themes).toEqual([]);
		}
		const custom: Theme = { ...PRESETS['kerf-dark'], name: 'Custom', colors: { ...PRESETS['kerf-dark'].colors, waveform: '#123456' } };
		const book = readBook(JSON.parse(themeJson(custom)));
		expect(book.themes.length).toBe(1);
		expect(book.active).toBe(book.themes[0].id);
		expect(book.themes[0].name).toBe('Custom');
		expect(book.themes[0].colors.waveform).toBe('#123456');
		expect(book.themes[0].base).toBeUndefined();
		expect(resolve(book, false).theme.colors).toEqual(custom.colors);
	});

	test('a built-in the user renamed keeps its name, as a copy that remembers what it was', () => {
		const book = readBook(JSON.parse(themeJson({ ...PRESETS['high-contrast'], name: 'My contrast' })));
		expect(book.themes[0].name).toBe('My contrast');
		expect(book.themes[0].base).toBe('high-contrast');
		expect(book.themes[0].colors).toEqual(PRESETS['high-contrast'].colors);
	});

	test('an untouched Kerf Light of an earlier build becomes the current Kerf Light', () => {
		for (const old of SUPERSEDED_KERF_LIGHT) {
			const book = readBook(JSON.parse(JSON.stringify({ ...PRESETS['kerf-light'], colors: old })));
			expect(book.active).toBe('kerf-light');
			expect(book.themes).toEqual([]);
		}
		// …but not one the user edited.
		const edited = readBook(JSON.parse(JSON.stringify({ ...PRESETS['kerf-light'], name: 'Mine', colors: { ...SUPERSEDED_KERF_LIGHT[0], waveform: '#123456' } })));
		expect(edited.themes[0].colors.waveform).toBe('#123456');
		expect(edited.themes[0].colors['kerf-500']).toBe(SUPERSEDED_KERF_LIGHT[0]['kerf-500']);
	});

	test('nothing, junk and a bad flat theme are the default book', () => {
		for (const raw of [null, undefined, 3, 'x', [], {}, { library: 5 }, { version: 1 }, { ...PRESETS['kerf-dark'], colors: [] }]) expect(readBook(raw)).toEqual(emptyBook());
	});

	test('a book from a file somebody edited keeps what is good and drops the rest, never throwing', () => {
		const good = (name: string, id: unknown) => ({ id, ...JSON.parse(themeJson({ ...PRESETS['kerf-dark'], name })) });
		const library = {
			version: 1,
			active: 'u-aaaaaaaa',
			follow: { on: true, light: 'u-aaaaaaaa', dark: 'kerf-light' },
			themes: [
				good('First', 'u-aaaaaaaa'),
				good('Second', 'u-aaaaaaaa'),
				good('Spoof', 'kerf-dark'),
				good('Odd id', '../../etc'),
				good('Kerf Light', 'u-bbbbbbbb'),
				{ id: 'u-cccccccc', name: 'Bad', version: 1, scheme: 'dark', colors: { 'kerf-500': 'red' } },
				'not a theme',
				null,
				good('', 'u-dddddddd')
			]
		};
		const book = readBook({ library });
		expect(book.themes.map((t) => t.name)).toEqual(['First', 'Second', 'Spoof', 'Odd id', 'Kerf Light 2']);
		const seen = [...PRESET_IDS, ...book.themes.map((t) => t.id)];
		expect(new Set(seen).size).toBe(seen.length);
		expect(book.themes.every((t) => /^u-[a-z0-9]{3,24}$/.test(t.id))).toBe(true);
		expect(book.themes[0].id).toBe('u-aaaaaaaa');
		expect(book.active).toBe('u-aaaaaaaa');
		// A slot holding a theme of the wrong scheme, or one that is not there, is its default.
		expect(book.follow).toEqual({ on: true, light: 'kerf-light', dark: 'kerf-dark' });
	});

	test('a file with a thousand themes keeps the first few and does not choke', () => {
		const themes = Array.from({ length: 1000 }, (_, i) => ({ id: `u-t${String(i).padStart(5, '0')}`, ...JSON.parse(themeJson({ ...PRESETS['kerf-dark'], name: `T${i}` })) }));
		expect(readBook({ library: { version: 1, themes } }).themes.length).toBe(MAX_THEMES);
	});

	test('an active theme that is not there, a follow that is not an object and a wrong version are the defaults', () => {
		const out = readBook({ library: { version: 1, active: 'u-gone', follow: 'yes', themes: 'no' } });
		expect(out).toEqual(emptyBook());
		expect(readBook({ ...PRESETS['kerf-light'], library: { version: 2, active: 'high-contrast' } }).active).toBe('kerf-light');
	});

	test('an id keeps the user themes apart from any built-in', () => {
		const user = readBook({ library: { version: 1, themes: [{ id: 'high-contrast', ...JSON.parse(themeJson(PRESETS['kerf-dark'])), name: 'Imposter' }] } }).themes[0] as UserTheme;
		expect(user.id).not.toBe('high-contrast');
		expect(lookup(emptyBook(), 'high-contrast')).toBe(PRESETS['high-contrast']);
	});
});

describe('the list a long library shows', () => {
	test('the user\'s themes are by name, case and numbers read as a person would, after the built-ins', () => {
		let book = emptyBook();
		for (const name of ['take 10', 'Take 2', 'Zebra', 'apple', 'Take 1']) {
			const out = add(book, { ...PRESETS['kerf-dark'], name }, counter());
			if (!out.ok) throw new Error(out.reason);
			book = out.book;
		}
		const names = entries(book).map((e) => e.theme.name);
		expect(names).toEqual(['Kerf Dark', 'Kerf Light', 'High contrast', 'apple', 'Take 1', 'Take 2', 'take 10', 'Zebra']);
		// What is stored keeps the order they were made in: the list is a view.
		expect(book.themes.map((t) => t.name)).toEqual(['take 10', 'Take 2', 'Zebra', 'apple', 'Take 1']);
	});

	test('the same name on two sorts the same way every time', () => {
		const a = mine(emptyBook(), 'kerf-dark');
		const book = { ...a.book, themes: [...a.book.themes, { ...a.book.themes[0], id: 'u-aaaaaaab' }, { ...a.book.themes[0], id: 'u-aaaaaaaa' }] };
		expect(entries(book).map((e) => e.id).slice(3)).toEqual([a.id, 'u-aaaaaaaa', 'u-aaaaaaab'].sort((x, y) => x.localeCompare(y)));
	});
});

describe('turning the system-follow off', () => {
	test('keeps the theme on screen, whichever scheme the system is in', () => {
		const dark = mine(emptyBook(), 'high-contrast');
		let book = setFollow(select(dark.book, dark.id), { on: true });
		expect(resolve(book, true).id).toBe(dark.id);
		expect(resolve(book, false).id).toBe('kerf-light');
		for (const systemDark of [true, false]) {
			const off = setFollow(book, { on: false }, systemDark);
			expect(resolve(off, systemDark).id, `system dark: ${systemDark}`).toBe(resolve(book, systemDark).id);
			expect(off.active).toBe(resolve(book, systemDark).id);
			expect(off.follow.on).toBe(false);
		}
	});

	test('turning it off when it was not on changes nothing, and the slots are kept for next time', () => {
		const book = emptyBook();
		expect(setFollow(book, { on: false })).toBe(book);
		const on = setFollow(book, { on: true });
		const off = setFollow(on, { on: false }, false);
		expect(off.follow.light).toBe('kerf-light');
		expect(off.follow.dark).toBe('kerf-dark');
	});
});

describe('using a theme while following the system, said before it is done', () => {
	const following = (systemDark: boolean) => {
		const book = setFollow(emptyBook(), { on: true });
		return { book, p: (id: string) => placement(book, id, systemDark) };
	};

	test('when not following it is in use, and there is nothing to say', () => {
		const p = placement(emptyBook(), 'kerf-light', false);
		expect(p).toEqual({ following: false, slot: null, replaced: null, unchanged: false, visible: true });
		expect(placementNote('Kerf Light', p)).toBeNull();
		expect(placement(emptyBook(), 'kerf-dark', false).unchanged).toBe(true);
	});

	test('a theme of the scheme the system is in is the one on show, and names what it replaces', () => {
		const { p } = following(true);
		expect(p('high-contrast')).toEqual({ following: true, slot: 'dark', replaced: 'Kerf Dark', unchanged: false, visible: true });
		const note = placementNote('High contrast', p('high-contrast'))!;
		expect(note.title).toBe('Set “High contrast” as the dark theme');
		expect(note.description).toBe('Kerf uses it whenever the system is dark. It replaces “Kerf Dark”.');
	});

	test('a theme of the other scheme says the page will not change until the system does', () => {
		const { p } = following(false);
		const note = placementNote('High contrast', p('high-contrast'))!;
		expect(p('high-contrast').visible).toBe(false);
		expect(note.title).toBe('Set “High contrast” as the dark theme');
		expect(note.description).toBe('Kerf uses it whenever the system is dark. It replaces “Kerf Dark”. The system is light now, so the page changes when it switches.');
		const m = mine(emptyBook(), 'kerf-light');
		const book = setFollow(m.book, { on: true });
		const note2 = placementNote('Mine', placement(book, m.id, true))!;
		expect(note2.title).toBe('Set “Mine” as the light theme');
		expect(note2.description).toBe('Kerf uses it whenever the system is light. It replaces “Kerf Light”. The system is dark now, so the page changes when it switches.');
	});

	test('a theme that already has the slot says so instead of looking inert', () => {
		const { p } = following(false);
		expect(p('kerf-dark')).toMatchObject({ unchanged: true, replaced: null });
		expect(placementNote('Kerf Dark', p('kerf-dark'))).toEqual({ title: '“Kerf Dark” is already the dark theme', description: 'Kerf uses it whenever the system is dark.' });
	});

	test('a theme that is not there changes nothing and says nothing', () => {
		expect(placementNote('x', placement(setFollow(emptyBook(), { on: true }), 'u-gone', true))).toBeNull();
	});
});

describe('changing a slotted theme\'s scheme', () => {
	const slotted = () => {
		const m = mine(emptyBook(), 'high-contrast');
		const book = setFollow(m.book, { on: true, dark: m.id });
		expect(book.follow.dark).toBe(m.id);
		return { book, id: m.id };
	};

	test('puts the slot back to its default at save time and says which', () => {
		const { book, id } = slotted();
		const out = update(book, id, { ...book.themes[0], scheme: 'light' });
		expect(out.ok && out.slotReset).toBe('dark');
		expect(out.ok && out.book.follow).toEqual({ on: true, light: 'kerf-light', dark: 'kerf-dark' });
		expect(out.ok && out.book.themes[0].scheme).toBe('light');
	});

	test('what is saved reads back as it is: nothing is silently reverted at the next launch', () => {
		const { book, id } = slotted();
		const out = update(book, id, { ...book.themes[0], scheme: 'light' });
		if (!out.ok) throw new Error();
		expect(readBook(JSON.parse(JSON.stringify(bookBlob(out.book, resolve(out.book, true).theme))))).toEqual(out.book);
	});

	test('a theme in no slot, a scheme that stays and a theme the follow is off for need no reset', () => {
		const { book, id } = slotted();
		expect(update(book, id, { ...book.themes[0], name: 'Renamed' })).toMatchObject({ ok: true, slotReset: null });
		const other = mine(emptyBook(), 'kerf-dark');
		expect(update(other.book, other.id, { ...other.book.themes[0], scheme: 'light' })).toMatchObject({ ok: true, slotReset: null });
	});

	test('a light theme in the light slot that becomes dark gives up the light slot', () => {
		const m = mine(emptyBook(), 'kerf-light');
		const book = setFollow(m.book, { on: true, light: m.id });
		const out = update(book, m.id, { ...book.themes[0], scheme: 'dark' });
		expect(out.ok && out.slotReset).toBe('light');
		expect(out.ok && out.book.follow.light).toBe('kerf-light');
	});
});

const withBook = () => {
	let book = emptyBook();
	const a = duplicate(book, 'kerf-dark', counter());
	if (!a.ok) throw new Error();
	book = setFollow(select(a.book, a.id), { on: true });
	return { book, id: a.id };
};

describe('a library from a newer build', () => {
	const newerLib = { version: LIBRARY_VERSION + 1, active: 'u-future', themes: [{ id: 'u-future', shapeless: true }], extra: { nested: [1, 2, 3] } };
	const stored = () => ({ ...JSON.parse(themeJson(PRESETS['kerf-light'])), library: newerLib });

	test('is not read: this build uses the flat theme beside it', () => {
		const out = readStored(JSON.parse(JSON.stringify(stored())));
		expect(out.book.active).toBe('kerf-light');
		expect(out.book.themes).toEqual([]);
		expect(out.newer).toEqual(newerLib);
	});

	test('is written back exactly as found, whatever this build writes beside it', () => {
		const { book, newer } = readStored(JSON.parse(JSON.stringify(stored())));
		const changed = select(book, 'high-contrast');
		const blob = bookBlob(changed, resolve(changed, false).theme, newer) as Record<string, unknown>;
		expect(blob.library).toBe(newer);
		expect(JSON.stringify(blob.library)).toBe(JSON.stringify(newerLib));
		expect(blob.name).toBe('High contrast');
	});

	test('only a version that is a number above ours is newer; anything else is read as before', () => {
		for (const version of [undefined, 'two', null, 1, 0, -3, NaN, [2]]) {
			const out = readStored({ ...JSON.parse(themeJson(PRESETS['kerf-light'])), library: { version, active: 'high-contrast' } });
			expect(out.newer, String(version)).toBeUndefined();
		}
		expect(readStored({ ...JSON.parse(themeJson(PRESETS['kerf-light'])), library: [2] }).newer).toBeUndefined();
		expect(readStored(null).newer).toBeUndefined();
	});

	test('the book this build holds is never the newer one\'s: a current library round-trips as before', () => {
		const { book } = withBook();
		const blob = JSON.parse(JSON.stringify(bookBlob(book, resolve(book, true).theme)));
		expect(readStored(blob).newer).toBeUndefined();
		expect(readStored(blob).book).toEqual(book);
	});
});

describe('the limit', () => {
	test('is said in full: how many, and what to do', () => {
		expect(LIMIT_REASON).toContain(String(MAX_THEMES));
		expect(LIMIT_REASON).toContain('delete one');
	});
});
