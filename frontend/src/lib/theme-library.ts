// The user's themes, as data. The built-in presets are always there and read-only; anything the user makes
// (a copy of one, an import) is a `UserTheme` with an id of its own. A `ThemeBook` says which theme is in use,
// and optionally that Kerf follows the system's light / dark setting with one theme assigned to each.
//
// Stored in the one opaque `Settings.theme` value, as the theme in force (flat, exactly what 0.31 wrote, so a
// build that only knows that reads it) with the book beside it under `library`. A flat theme with no `library`
// is a 0.31 file: it becomes the active theme here (`readBook`). Everything is read field by field and a value
// that is not what it should be falls back rather than failing the rest.

import { PRESETS, PRESET_IDS, cleanName, parseTheme, presetIdFor, themeFields, upgradeStoredTheme, NAME_MAX, type PresetId, type Scheme, type Theme } from './theme';

/** A theme of the user's: the file's fields, and an id this build gave it. */
export type UserTheme = Theme & { id: string };

export interface Follow {
	/** Use the theme assigned to the system's current scheme instead of the active one. */
	on: boolean;
	light: string;
	dark: string;
}

export interface ThemeBook {
	/** The theme in use when not following the system: a built-in's id or a user theme's. */
	active: string;
	themes: UserTheme[];
	follow: Follow;
}

/** More than this is a file that is not a person's themes. */
export const MAX_THEMES = 40;

/** Why a copy or an import is refused at the limit. */
export const LIMIT_REASON = `You have ${MAX_THEMES} themes of your own, the most Kerf keeps; delete one first.`;

export const DEFAULT_FOLLOW: Follow = { on: false, light: 'kerf-light', dark: 'kerf-dark' };

export function emptyBook(): ThemeBook {
	return { active: 'kerf-dark', themes: [], follow: { ...DEFAULT_FOLLOW } };
}

export const isBuiltIn = (id: string): id is PresetId => (PRESET_IDS as string[]).includes(id);

/** The shape of an id this build hands out. */
const USER_ID = /^u-[a-z0-9]{3,24}$/;

export function lookup(book: ThemeBook, id: string): Theme | null {
	if (isBuiltIn(id)) return PRESETS[id];
	return book.themes.find((t) => t.id === id) ?? null;
}

export interface ThemeEntry {
	id: string;
	theme: Theme;
	builtIn: boolean;
}

/** Every theme the user can pick, as the list shows them: the built-ins, then their own by name (case and digits as a
 *  person reads them: "Take 2" before "Take 10"). */
export function entries(book: ThemeBook): ThemeEntry[] {
	const byName = (a: UserTheme, b: UserTheme) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base', numeric: true }) || a.id.localeCompare(b.id);
	return [
		...PRESET_IDS.map((id) => ({ id, theme: PRESETS[id] as Theme, builtIn: true })),
		...[...book.themes].sort(byName).map((t) => ({ id: t.id, theme: t as Theme, builtIn: false }))
	];
}

/** The themes a scheme's slot may hold. */
export function entriesFor(book: ThemeBook, scheme: Scheme): ThemeEntry[] {
	return entries(book).filter((e) => e.theme.scheme === scheme);
}

/** The theme in force: the active one, or with `follow.on` the one assigned to the system's scheme. A theme that has gone
 *  (or an id that was never one) is Kerf Dark, so the app always has a theme. */
export function resolve(book: ThemeBook, systemDark: boolean): { id: string; theme: Theme } {
	const id = book.follow.on ? (systemDark ? book.follow.dark : book.follow.light) : book.active;
	const theme = lookup(book, id);
	return theme ? { id, theme } : { id: 'kerf-dark', theme: PRESETS['kerf-dark'] };
}

// ---- names and ids ------------------------------------------------------------------------------

/** Names are compared the way a person would: whatever the case, whatever the spacing. */
const key = (name: string) => cleanName(name).toLowerCase();

/** The names in use, the built-ins' included. */
export function takenNames(book: ThemeBook, except?: string): string[] {
	return entries(book)
		.filter((e) => e.id !== except)
		.map((e) => e.theme.name);
}

/** `name` if it is free, else `name 2`, `name 3` … (a name never grows past `NAME_MAX`). */
export function uniqueName(name: string, taken: readonly string[]): string {
	const used = new Set(taken.map(key));
	const base = cleanName(name) || 'Theme';
	if (!used.has(key(base))) return base;
	for (let n = 2; ; n++) {
		const suffix = ` ${n}`;
		const candidate = `${Array.from(base).slice(0, NAME_MAX - suffix.length).join('').trim()}${suffix}`;
		if (!used.has(key(candidate))) return candidate;
	}
}

/** A name for a copy of `name`: `Kerf Dark copy`, `Kerf Dark copy 2` … A copy of a copy is another copy of the
 *  original, not "copy copy". */
export function copyName(name: string, taken: readonly string[]): string {
	const room = NAME_MAX - ' copy 99'.length;
	const stem = Array.from(cleanName(name).replace(/\s+copy(\s+\d+)?$/i, '') || 'Theme').slice(0, room).join('').trim();
	return uniqueName(`${stem} copy`, taken);
}

/** An id no theme in `taken` has. `random` is injected so a test can count on it. */
export function newId(taken: readonly string[], random: () => number = Math.random): string {
	for (;;) {
		const id = `u-${Math.floor(random() * 36 ** 8)
			.toString(36)
			.padStart(8, '0')}`;
		if (!taken.includes(id)) return id;
	}
}

// ---- operations ---------------------------------------------------------------------------------
// Each returns a new book. One that cannot be done says why instead of doing something near it.

export type Result<T> = ({ ok: true } & T) | { ok: false; reason: string };

/** Use a theme. While following the system a pick assigns it to its own scheme's slot instead: it is what is used when the system
 *  is that scheme. An id that is not a theme changes nothing. */
export function select(book: ThemeBook, id: string): ThemeBook {
	const theme = lookup(book, id);
	if (!theme) return book;
	if (book.follow.on) {
		const slot = theme.scheme === 'light' ? 'light' : 'dark';
		return book.follow[slot] === id ? book : { ...book, follow: { ...book.follow, [slot]: id } };
	}
	return book.active === id ? book : { ...book, active: id };
}

/** A copy of any theme, as a user theme the book then has. `base` is the built-in the copy came from, so a colour can
 *  be put back to it. */
export function duplicate(book: ThemeBook, id: string, random?: () => number): Result<{ book: ThemeBook; id: string }> {
	const from = lookup(book, id);
	if (!from) return { ok: false, reason: 'That theme is gone.' };
	if (book.themes.length >= MAX_THEMES) return { ok: false, reason: LIMIT_REASON };
	const copy: UserTheme = {
		...from,
		id: newId(entries(book).map((e) => e.id), random),
		name: copyName(from.name, takenNames(book)),
		base: isBuiltIn(id) ? id : from.base,
		colors: { ...from.colors },
		shape: { ...from.shape }
	};
	return { ok: true, book: { ...book, themes: [...book.themes, copy] }, id: copy.id };
}

/** A theme that came from outside (an import): added under a name that is free. Not "upgraded": a file is what it says. */
export function add(book: ThemeBook, theme: Theme, random?: () => number): Result<{ book: ThemeBook; id: string }> {
	if (book.themes.length >= MAX_THEMES) return { ok: false, reason: LIMIT_REASON };
	const user: UserTheme = {
		...theme,
		id: newId(entries(book).map((e) => e.id), random),
		name: uniqueName(theme.name, takenNames(book))
	};
	return { ok: true, book: { ...book, themes: [...book.themes, user] }, id: user.id };
}

export function rename(book: ThemeBook, id: string, name: string): Result<{ book: ThemeBook }> {
	if (isBuiltIn(id)) return { ok: false, reason: "A built-in theme can't be renamed. Make a copy first." };
	const target = book.themes.find((t) => t.id === id);
	if (!target) return { ok: false, reason: 'That theme is gone.' };
	const clean = cleanName(name);
	if (clean === '') return { ok: false, reason: 'A theme needs a name.' };
	if (takenNames(book, id).some((n) => key(n) === key(clean))) return { ok: false, reason: `There is already a theme called “${clean}”.` };
	if (clean === target.name) return { ok: true, book };
	return { ok: true, book: { ...book, themes: book.themes.map((t) => (t.id === id ? { ...t, name: clean } : t)) } };
}

/** Replace a user theme's content (the editor's Save), keeping its id. Its name is kept free of the others' like `rename`. A theme
 *  that is a system-follow slot's and has been given the other scheme leaves the slot, which goes back to its default
 *  (`slotReset` says which): a slot holds a theme of its scheme, and the next launch would take it away anyway. */
export function update(book: ThemeBook, id: string, theme: Theme): Result<{ book: ThemeBook; slotReset: Scheme | null }> {
	if (isBuiltIn(id)) return { ok: false, reason: "A built-in theme can't be changed. Make a copy first." };
	const old = book.themes.find((t) => t.id === id);
	if (!old) return { ok: false, reason: 'That theme is gone.' };
	const clean = cleanName(theme.name);
	if (clean === '') return { ok: false, reason: 'A theme needs a name.' };
	if (takenNames(book, id).some((n) => key(n) === key(clean))) return { ok: false, reason: `There is already a theme called “${clean}”.` };
	const next: UserTheme = { ...theme, id, name: clean };
	const follow = { ...book.follow };
	let slotReset: Scheme | null = null;
	for (const slot of ['light', 'dark'] as const) {
		if (follow[slot] === id && next.scheme !== slot) {
			follow[slot] = DEFAULT_FOLLOW[slot];
			slotReset = slot;
		}
	}
	return { ok: true, book: { ...book, themes: book.themes.map((t) => (t.id === id ? next : t)), follow }, slotReset };
}

/** Delete a user theme. What used it falls back to the built-in of its scheme: the active theme to Kerf Dark or Kerf Light
 *  (whichever the deleted one was), a slot of the system-follow to its default. */
export function remove(book: ThemeBook, id: string): Result<{ book: ThemeBook }> {
	if (isBuiltIn(id)) return { ok: false, reason: "A built-in theme can't be deleted." };
	const gone = book.themes.find((t) => t.id === id);
	if (!gone) return { ok: false, reason: 'That theme is gone.' };
	const fallback = gone.scheme === 'light' ? 'kerf-light' : 'kerf-dark';
	return {
		ok: true,
		book: {
			active: book.active === id ? fallback : book.active,
			themes: book.themes.filter((t) => t.id !== id),
			follow: {
				on: book.follow.on,
				light: book.follow.light === id ? DEFAULT_FOLLOW.light : book.follow.light,
				dark: book.follow.dark === id ? DEFAULT_FOLLOW.dark : book.follow.dark
			}
		}
	};
}

/** Change the system-follow setting. A slot is only given a theme of its own scheme. Turning it on starts the slot of the
 *  theme in use from that theme, so it stays in use if the system is already in its scheme. Turning it off keeps what is on
 *  screen: the theme the system's scheme had selected becomes the active one (it would otherwise be whichever was active
 *  before following began, and the page would change under the user's hand). */
export function setFollow(book: ThemeBook, patch: Partial<Follow>, systemDark = false): ThemeBook {
	const follow = { ...book.follow };
	let active = book.active;
	if (patch.on === true && !follow.on) {
		const inUse = lookup(book, book.active);
		if (inUse) follow[inUse.scheme] = book.active;
	}
	if (patch.on === false && follow.on) active = resolve(book, systemDark).id;
	for (const slot of ['light', 'dark'] as const) {
		const id = patch[slot];
		if (id !== undefined && lookup(book, id)?.scheme === slot) follow[slot] = id;
	}
	if (patch.on !== undefined) follow.on = patch.on;
	const same = book.follow.on === follow.on && book.follow.light === follow.light && book.follow.dark === follow.dark && book.active === active;
	return same ? book : { ...book, active, follow };
}

/** What using a theme does to the book, said before it is done: while following the system a theme is not "in use" but is the
 *  light or the dark one, which may replace another and may not show until the system changes scheme. */
export interface Placement {
	following: boolean;
	/** The slot it goes in (its own scheme), when following. */
	slot: Scheme | null;
	/** The name of the theme it takes the slot from. */
	replaced: string | null;
	/** It already has the slot (or, not following, is already the active theme). */
	unchanged: boolean;
	/** It is what the page shows once chosen: always when not following, else when the system is in the slot's scheme. */
	visible: boolean;
}

export function placement(book: ThemeBook, id: string, systemDark: boolean): Placement {
	const theme = lookup(book, id);
	if (!theme || !book.follow.on) return { following: false, slot: null, replaced: null, unchanged: book.active === id, visible: true };
	const slot = theme.scheme;
	const held = book.follow[slot];
	return {
		following: true,
		slot,
		replaced: held === id ? null : (lookup(book, held)?.name ?? null),
		unchanged: held === id,
		visible: (systemDark ? 'dark' : 'light') === slot
	};
}

/** The sentence for a pick made while following the system (null when not following: the page changing is the answer). */
export function placementNote(name: string, p: Placement): { title: string; description: string } | null {
	if (!p.following || !p.slot) return null;
	const used = `Kerf uses it whenever the system is ${p.slot}.`;
	if (p.unchanged) return { title: `“${name}” is already the ${p.slot} theme`, description: used };
	return {
		title: `Set “${name}” as the ${p.slot} theme`,
		description: [used, p.replaced ? `It replaces “${p.replaced}”.` : '', p.visible ? '' : `The system is ${p.slot === 'dark' ? 'light' : 'dark'} now, so the page changes when it switches.`]
			.filter(Boolean)
			.join(' ')
	};
}

// ---- stored form ----------------------------------------------------------------------------------

/** The user themes of a stored `library`, and the rest of the book around them. */
function readLibrary(raw: unknown): ThemeBook | null {
	if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return null;
	const r = raw as Record<string, unknown>;
	if (r.version !== 1) return null;
	const themes: UserTheme[] = [];
	const ids = new Set<string>(PRESET_IDS);
	const names = new Set<string>(PRESET_IDS.map((id) => key(PRESETS[id].name)));
	if (Array.isArray(r.themes)) {
		for (const item of r.themes.slice(0, MAX_THEMES)) {
			const theme = parseTheme(item);
			if (!theme) continue;
			const given = typeof (item as Record<string, unknown>).id === 'string' ? ((item as Record<string, unknown>).id as string) : '';
			// An id this build would not hand out, or one already taken, is replaced; the theme is not lost for it.
			const id = USER_ID.test(given) && !ids.has(given) ? given : newId([...ids]);
			ids.add(id);
			const name = uniqueName(theme.name, [...names]);
			names.add(key(name));
			themes.push({ ...theme, id, name });
		}
	}
	const book: ThemeBook = { active: 'kerf-dark', themes, follow: { ...DEFAULT_FOLLOW } };
	if (typeof r.active === 'string' && lookup(book, r.active)) book.active = r.active;
	const f = typeof r.follow === 'object' && r.follow !== null && !Array.isArray(r.follow) ? (r.follow as Record<string, unknown>) : {};
	book.follow.on = f.on === true;
	for (const slot of ['light', 'dark'] as const) {
		const id = f[slot];
		if (typeof id === 'string' && lookup(book, id)?.scheme === slot) book.follow[slot] = id;
	}
	return book;
}

/** The newest `library` this build can read. */
export const LIBRARY_VERSION = 1;

/** A `library` written by a newer build: kept exactly as found when the settings are written, never read and never replaced. */
function newerLibrary(raw: unknown): unknown {
	if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return undefined;
	const lib = (raw as Record<string, unknown>).library;
	if (typeof lib !== 'object' || lib === null || Array.isArray(lib)) return undefined;
	const v = (lib as Record<string, unknown>).version;
	return typeof v === 'number' && Number.isFinite(v) && v > LIBRARY_VERSION ? lib : undefined;
}

/** What the stored `theme` value holds: a book (`library`), a flat theme from before there were books, or nothing usable. A flat
 *  theme is brought up to date as 0.31 would have (`upgradeStoredTheme`); if it is exactly a built-in, under that name, it
 *  is that built-in, and otherwise a theme of the user's (a copy they edited, or renamed). A `library` of a newer version is not
 *  read: the flat theme beside it is what this build uses, and `newer` carries the library to be written back untouched. */
export function readStored(raw: unknown): { book: ThemeBook; newer: unknown } {
	const newer = newerLibrary(raw);
	if (newer === undefined && typeof raw === 'object' && raw !== null && !Array.isArray(raw)) {
		const lib = readLibrary((raw as Record<string, unknown>).library);
		if (lib) return { book: lib, newer };
	}
	const flat = parseTheme(raw);
	if (!flat) return { book: emptyBook(), newer };
	const theme = upgradeStoredTheme(flat);
	const preset = presetIdFor(theme);
	const book = emptyBook();
	if (preset !== 'custom' && key(theme.name) === key(PRESETS[preset].name)) return { book: { ...book, active: preset }, newer };
	const user: UserTheme = { ...theme, id: 'u-legacy', name: uniqueName(theme.name, takenNames(book)) };
	// A renamed copy of a built-in is put back to it; a mix of the user's own to its scheme's.
	if (preset !== 'custom') user.base = preset;
	return { book: { ...book, themes: [user], active: user.id }, newer };
}

export const readBook = (raw: unknown): ThemeBook => readStored(raw).book;

/** What goes in the settings file: the theme in force as a flat theme (what a build that has no books reads), and the
 *  book beside it — or, when the file held a library of a newer version (`newer`), that library exactly as it was. */
export function bookBlob(book: ThemeBook, inForce: Theme, newer?: unknown): unknown {
	return {
		...themeFields(inForce),
		library:
			newer !== undefined
				? newer
				: {
						version: LIBRARY_VERSION,
						active: book.active,
						follow: { ...book.follow },
						themes: book.themes.map((t) => ({ id: t.id, ...themeFields(t) }))
					}
	};
}
