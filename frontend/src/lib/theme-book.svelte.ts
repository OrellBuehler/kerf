// The user's themes and the editing of one (Svelte 5 runes). Owned by `settings` (`settings.themes`); the logic is
// in the pure modules (`theme-library.ts` the book, `theme-session.ts` one sitting in the editor, `theme-file.ts`
// the file), this holds the live copy, puts the theme in force on the page and writes the book through.
//
// What is in force: the draft while the editor is open, otherwise the active theme — or, with system-follow on,
// the one assigned to the system's current scheme. The draft is applied as it is edited and written nowhere
// until Save; Cancel puts back what was there. Read once (a view coming back from another write never replaces
// it) and written one at a time, newest wins.

import { confirmAction, exportThemeFile, importThemeFile } from './api';
import { CONFIRM } from './confirm-copy';
import { toast } from './notifications.svelte';
import { singleFlight } from './single-flight';
import { PRESETS, applyTheme, themeJson, type Theme } from './theme';
import { readThemeFile, themeFileName } from './theme-file';
import * as lib from './theme-library';
import * as sit from './theme-session';

export class ThemeBookState {
	book = $state.raw<lib.ThemeBook>(lib.emptyBook());
	/** The open sitting in the theme editor, or null. */
	session = $state.raw<sit.Session | null>(null);
	/** Whether the system is in its dark scheme (`prefers-color-scheme`). */
	systemDark = $state(false);
	/** The theme in force: what the page is drawn in. */
	current = $state.raw<Theme>(PRESETS['kerf-dark']);

	/** The book with the copy a built-in's Customize made, until it is kept: nothing is added to the library by opening the editor. */
	private pending: lib.ThemeBook | null = null;
	private watching = false;
	private readonly store: (blob: unknown) => Promise<void>;
	private readonly writer = singleFlight(async () => {
		await this.store(lib.bookBlob(this.book, lib.resolve(this.book, this.systemDark).theme));
	});

	/** `store` writes the opaque `theme` setting; a failure is its own to report. */
	constructor(store: (blob: unknown) => Promise<void>) {
		this.store = store;
	}

	/** Take what the settings file holds. Run once; `forget` before a second time (a reset). */
	read(raw: unknown) {
		this.book = lib.readBook(raw);
		this.session = null;
		this.watch();
		this.refresh();
	}

	/** Back to the built-in default, for a reset that is about to read the fresh view. */
	forget() {
		this.book = lib.emptyBook();
		this.session = null;
		this.pending = null;
		this.refresh();
	}

	/** Hear the system change its scheme (once). */
	private watch() {
		if (this.watching || typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
		this.watching = true;
		const query = window.matchMedia('(prefers-color-scheme: dark)');
		this.systemDark = query.matches;
		query.addEventListener?.('change', (e) => {
			this.systemDark = e.matches;
			this.refresh();
		});
	}

	/** Put the theme in force on the page. Called after every change to the book, the session or the system's scheme. */
	private refresh() {
		const theme = this.session ? this.session.draft : lib.resolve(this.book, this.systemDark).theme;
		this.current = theme;
		applyTheme(theme);
	}

	private commit(book: lib.ThemeBook) {
		if (book === this.book) return;
		this.book = book;
		this.writer.request();
		this.refresh();
	}

	// ---- reading the book -----------------------------------------------------------------------

	get entries(): lib.ThemeEntry[] {
		return lib.entries(this.book);
	}

	/** The theme in force as the list marks it: an id (the draft being edited is its theme's). */
	get inForceId(): string {
		return lib.resolve(this.book, this.systemDark).id;
	}

	get follow(): lib.Follow {
		return this.book.follow;
	}

	get canAdd(): boolean {
		return this.book.themes.length < lib.MAX_THEMES;
	}

	// ---- the library ------------------------------------------------------------------------------

	/** Use a theme now; while following the system, assign it to its own scheme. */
	select(id: string) {
		this.commit(lib.select(this.book, id));
	}

	setFollow(patch: Partial<lib.Follow>) {
		this.commit(lib.setFollow(this.book, patch));
	}

	/** A copy of any theme, as the user's own. Returns its id, or null with the reason said. */
	duplicate(id: string): string | null {
		const out = lib.duplicate(this.book, id);
		if (!out.ok) {
			toast.error("Couldn't make a copy", { description: out.reason });
			return null;
		}
		this.commit(out.book);
		return out.id;
	}

	/** Rename one of the user's own; says why when it cannot. */
	rename(id: string, name: string): { ok: true } | { ok: false; reason: string } {
		const out = lib.rename(this.book, id, name);
		if (!out.ok) return out;
		this.commit(out.book);
		return { ok: true };
	}

	/** Delete one of the user's own, after asking. */
	async remove(id: string): Promise<boolean> {
		const theme = lib.lookup(this.book, id);
		if (!theme || lib.isBuiltIn(id)) return false;
		const yes = await confirmAction(`“${theme.name}” is deleted for good. A theme file you exported of it still works.`, CONFIRM.deleteTheme.title, CONFIRM.deleteTheme);
		if (!yes) return false;
		const out = lib.remove(this.book, id);
		if (!out.ok) {
			toast.error("Couldn't delete the theme", { description: out.reason });
			return false;
		}
		this.commit(out.book);
		return true;
	}

	/** Choose a file, add it to the library and use it. */
	async importFile(): Promise<boolean> {
		let text: string | null;
		try {
			text = await importThemeFile();
		} catch (e) {
			toast.error("Couldn't read the file", { description: String(e) });
			return false;
		}
		if (text == null) return false;
		const file = readThemeFile(text);
		if (!file.ok) {
			toast.error("That isn't a Kerf theme", { description: file.reason });
			return false;
		}
		const out = lib.add(this.book, file.theme);
		if (!out.ok) {
			toast.error("Couldn't import the theme", { description: out.reason });
			return false;
		}
		this.commit(lib.select(out.book, out.id));
		toast.success(`Imported “${lib.lookup(out.book, out.id)?.name}”`, { description: 'It is in use now; your other themes are still in the list.' });
		return true;
	}

	/** Save one theme as a file. */
	async exportFile(id: string): Promise<void> {
		const theme = lib.lookup(this.book, id);
		if (!theme) return;
		try {
			const where = await exportThemeFile(themeJson(theme), themeFileName(theme.name));
			if (where) toast.success('Theme saved', { description: where });
		} catch (e) {
			toast.error("Couldn't save the theme", { description: String(e) });
		}
	}

	// ---- editing ------------------------------------------------------------------------------------

	/** Open the editor on a theme. A built-in cannot be changed, so a copy of it is what opens; the copy joins the library, and is
	 *  used, only when it is saved. */
	edit(id: string): boolean {
		if (this.session) return false;
		let target = id;
		this.pending = null;
		if (lib.isBuiltIn(id)) {
			const out = lib.duplicate(this.book, id);
			if (!out.ok) {
				toast.error("Couldn't make a copy", { description: out.reason });
				return false;
			}
			this.pending = out.book;
			target = out.id;
		}
		const theme = lib.lookup(this.pending ?? this.book, target);
		if (!theme) return false;
		this.session = sit.begin(target, theme);
		this.refresh();
		return true;
	}

	/** The theme the editor is on. */
	get editing(): sit.Session | null {
		return this.session;
	}

	get dirty(): boolean {
		return this.session !== null && sit.dirty(this.session);
	}

	private apply(next: (s: sit.Session) => sit.Session) {
		const s = this.session;
		if (!s) return;
		const out = next(s);
		if (out === s) return;
		this.session = out;
		this.refresh();
	}

	setColor(token: Parameters<typeof sit.setColor>[1], hex: string) {
		this.apply((s) => sit.setColor(s, token, hex));
	}
	setShape(token: Parameters<typeof sit.setShape>[1], value: number) {
		this.apply((s) => sit.setShape(s, token, value));
	}
	setChoice<K extends Parameters<typeof sit.setChoice>[1]>(field: K, value: Parameters<typeof sit.setChoice<K>>[2]) {
		this.apply((s) => sit.setChoice(s, field, value));
	}
	setName(name: string) {
		this.apply((s) => sit.setName(s, name));
	}
	setScheme(scheme: Theme['scheme']) {
		this.apply((s) => sit.setScheme(s, scheme));
	}
	resetColor(token: Parameters<typeof sit.resetColor>[1]) {
		this.apply((s) => sit.resetColor(s, token));
	}
	resetShape(field: Parameters<typeof sit.resetShape>[1]) {
		this.apply((s) => sit.resetShape(s, field));
	}
	revertAll() {
		this.apply((s) => sit.revertAll(s));
	}
	undo() {
		this.apply(sit.undo);
	}
	redo() {
		this.apply(sit.redo);
	}
	/** The picker closed or the field lost focus: the next change is a step of its own. */
	endEdit() {
		this.apply(sit.endEdit);
	}

	/** Keep the draft: the theme is replaced in the book and written (a copy of a built-in joins it, and is used). False, with the
	 *  reason said, when it cannot be (no name). */
	save(): boolean {
		const s = this.session;
		if (!s) return false;
		if (!this.pending && !sit.dirty(s)) {
			// Nothing was changed: nothing to write.
			this.session = null;
			this.refresh();
			return true;
		}
		const out = lib.update(this.pending ?? this.book, s.id, s.draft);
		if (!out.ok) {
			toast.error("Couldn't save the theme", { description: out.reason });
			return false;
		}
		const next = this.pending ? lib.select(out.book, s.id) : out.book;
		this.session = null;
		this.pending = null;
		this.commit(next);
		this.refresh();
		return true;
	}

	/** Close the editor and put back what was there. */
	cancel() {
		if (!this.session) return;
		this.session = null;
		this.pending = null;
		this.refresh();
	}

	/** Close the editor the way Escape and the close button do: straight away when nothing changed, else after asking. */
	async close(): Promise<boolean> {
		if (!this.session) return true;
		if (this.dirty) {
			const discard = await confirmAction(CONFIRM.discardThemeEdits.message, CONFIRM.discardThemeEdits.title, CONFIRM.discardThemeEdits);
			if (!discard) return false;
		}
		this.cancel();
		return true;
	}
}
