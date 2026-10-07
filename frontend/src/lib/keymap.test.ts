import { describe, expect, test } from 'bun:test';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';
import {
	ACTIONS,
	GROUPS,
	KEYMAP_VERSION,
	allowsRepeat,
	applyRebind,
	applyReset,
	chordFromEvent,
	conflictsFor,
	detectPlatform,
	differsFromDefault,
	displayChord,
	emptyOverrides,
	filterActions,
	findCollisions,
	formatChord,
	matchAction,
	parseChord,
	parseKeyOverrides,
	rebindConflicts,
	resetAction,
	resetAll,
	resetPlan,
	resolveBindings,
	sameChord,
	serializeOverrides,
	setActionChords,
	type ActionDef,
	type Chord,
	type KeyEventLike,
	type KeyOverrides,
	type Migration,
	type Platform
} from './keymap';

const PLATFORMS: Platform[] = ['mac', 'other'];

interface Mods {
	ctrl?: boolean;
	meta?: boolean;
	alt?: boolean;
	shift?: boolean;
}

const ev = (key: string, m: Mods = {}, code?: string): KeyEventLike => ({
	key,
	code,
	ctrlKey: !!m.ctrl,
	metaKey: !!m.meta,
	altKey: !!m.alt,
	shiftKey: !!m.shift
});

/** The platform's primary modifier (⌘ on a Mac, Ctrl elsewhere). */
const primary = (p: Platform): Mods => (p === 'mac' ? { meta: true } : { ctrl: true });

const chord = (text: string, p: Platform = 'other'): Chord => {
	const c = parseChord(text, p);
	if (!c) throw new Error(`not a chord: ${text}`);
	return c;
};

describe('platform', () => {
	test('a Mac is recognised from whichever field the webview fills in', () => {
		expect(detectPlatform({ platform: 'MacIntel' })).toBe('mac');
		expect(detectPlatform({ userAgentData: { platform: 'macOS' }, platform: 'x' })).toBe('mac');
		expect(detectPlatform({ userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)' })).toBe('mac');
		expect(detectPlatform({ platform: 'Win32' })).toBe('other');
		expect(detectPlatform({ platform: 'Linux x86_64' })).toBe('other');
		expect(detectPlatform({})).toBe('other');
	});
});

describe('parsing and writing chords', () => {
	test('Mod is the platform primary modifier', () => {
		expect(chord('Mod+Z', 'mac')).toEqual({ key: 'z', ctrl: false, meta: true, alt: false, shift: false });
		expect(chord('Mod+Z', 'other')).toEqual({ key: 'z', ctrl: true, meta: false, alt: false, shift: false });
	});

	test('the stored spelling is canonical whatever it was typed as', () => {
		expect(formatChord(chord('ctrl+shift+z', 'other'), 'other')).toBe('Mod+Shift+Z');
		expect(formatChord(chord('cmd+shift+z', 'mac'), 'mac')).toBe('Mod+Shift+Z');
		expect(formatChord(chord('Shift+Mod+Z', 'other'), 'other')).toBe('Mod+Shift+Z');
		expect(formatChord(chord('shift+left', 'other'), 'other')).toBe('Shift+ArrowLeft');
		expect(formatChord(chord('del'), 'other')).toBe('Delete');
		expect(formatChord(chord('f5'), 'other')).toBe('F5');
	});

	test('the non-primary Ctrl / Meta stay distinct from Mod', () => {
		// On a Mac Control is its own key; on Windows / Linux it is the Win key.
		expect(formatChord(chord('ctrl+k', 'mac'), 'mac')).toBe('Ctrl+K');
		expect(formatChord(chord('mod+ctrl+k', 'mac'), 'mac')).toBe('Mod+Ctrl+K');
		expect(formatChord(chord('meta+k', 'other'), 'other')).toBe('Meta+K');
		expect(formatChord(chord('ctrl+k', 'other'), 'other')).toBe('Mod+K');
	});

	test('punctuation keys, including + and a modified +', () => {
		expect(formatChord(chord('Mod+,'), 'other')).toBe('Mod+,');
		expect(chord('+').key).toBe('+');
		expect(chord('Mod++').key).toBe('+');
		expect(chord('Mod++').ctrl).toBe(true);
		expect(formatChord(chord('-'), 'other')).toBe('-');
	});

	test('what is not a chord', () => {
		for (const bad of ['', '   ', 'Shift+', 'Mod+Shift', 'Banana', 'Mod+Banana', 'F25', 'F0', 'Alt', '\u0007']) {
			expect(parseChord(bad, 'other')).toBeNull();
		}
	});

	test('a chord survives write then read on its own platform', () => {
		for (const p of PLATFORMS) {
			for (const text of ['Mod+Shift+Z', 'Mod+Alt+ArrowLeft', 'Shift+Delete', 'F5', 'Space', ',', '+', 'Mod+,']) {
				const c = chord(text, p);
				expect(parseChord(formatChord(c, p), p)).toEqual(c);
			}
		}
	});

	test('display reads the way the platform writes shortcuts', () => {
		expect(displayChord(chord('Mod+Shift+Z', 'mac'), 'mac')).toBe('⇧⌘Z');
		expect(displayChord(chord('Mod+Shift+Z', 'other'), 'other')).toBe('Ctrl+Shift+Z');
		expect(displayChord(chord('Mod+Alt+Ctrl+K', 'mac'), 'mac')).toBe('⌃⌥⌘K');
		expect(displayChord(chord('Delete'), 'other')).toBe('Del');
		expect(displayChord(chord('Backspace', 'mac'), 'mac')).toBe('⌫');
		expect(displayChord(chord('Backspace'), 'other')).toBe('Backspace');
		expect(displayChord(chord('Shift+ArrowLeft'), 'other')).toBe('Shift+←');
		expect(displayChord(chord('Space'), 'other')).toBe('Space');
		expect(displayChord(chord('Mod+,', 'mac'), 'mac')).toBe('⌘,');
		expect(displayChord(chord('Escape'), 'other')).toBe('Esc');
	});
});

describe('the chord a keypress is', () => {
	test('a letter is the letter, however it was typed', () => {
		expect(chordFromEvent(ev('z'))).toEqual(chord('Z'));
		// Caps Lock: the key says Z but nobody held Shift.
		expect(chordFromEvent(ev('Z'))).toEqual(chord('Z'));
		expect(chordFromEvent(ev('Z', { shift: true }))).toEqual(chord('Shift+Z'));
	});

	test('Space and the named keys', () => {
		expect(chordFromEvent(ev(' '))).toEqual(chord('Space'));
		expect(chordFromEvent(ev('ArrowLeft', { shift: true }))).toEqual(chord('Shift+ArrowLeft'));
		expect(chordFromEvent(ev('Delete'))).toEqual(chord('Delete'));
		expect(chordFromEvent(ev('F5'))).toEqual(chord('F5'));
	});

	test('the Shift of a shifted punctuation key is the character, not a modifier', () => {
		// `+` is Shift+= on a US keyboard and a bare key on a German one.
		expect(chordFromEvent(ev('+', { shift: true }))).toEqual(chord('+'));
		expect(chordFromEvent(ev('+'))).toEqual(chord('+'));
		expect(chordFromEvent(ev('=', { shift: false }))).toEqual(chord('='));
		// …but not a digit or a letter, where Shift is a real choice.
		expect(chordFromEvent(ev('1', { shift: true }))?.shift).toBe(true);
	});

	test('a layout without ASCII letters still gets its shortcuts', () => {
		expect(chordFromEvent(ev('я', { ctrl: true }, 'KeyZ'))).toEqual(chord('Mod+Z', 'other'));
		// A macOS Option combination types a symbol; the key under it is Z.
		expect(chordFromEvent(ev('Ω', { alt: true }, 'KeyZ'))).toEqual(chord('Alt+Z'));
		expect(chordFromEvent(ev('Dead', {}, 'Backquote'))).toEqual(chord('`'));
	});

	test('a space is Space however the platform types it', () => {
		// macOS types a no-break space for ⌥Space; written down as `Alt+ ` it could
		// not be read back, and the binding vanished on the next launch.
		for (const key of [' ', '\u00a0', '\u2003']) {
			const c = chordFromEvent({ ...ev(key, { alt: true }), code: 'Space' });
			expect(c).toEqual(chord('Alt+Space'));
			expect(formatChord(c!, 'mac')).toBe('Alt+Space');
			expect(parseChord(formatChord(c!, 'mac'), 'mac')).toEqual(c);
		}
		expect(chordFromEvent(ev('\u00a0', { shift: true }))).toEqual(chord('Shift+Space'));
	});

	test('a key that would not survive being written down is not a chord', () => {
		// `ß` upper-cases to `SS`, the Turkish dotless ı to an `I` that reads back as `i`.
		expect(chordFromEvent(ev('ß'))).toBeNull();
		expect(chordFromEvent(ev('ı', {}, 'KeyI'))).toBeNull();
		expect(chordFromEvent(ev('İ', { shift: true }, 'KeyI'))).toBeNull();
		// Whatever does come back is a chord that reads back as itself.
		const keys = ['ß', 'ı', 'ö', 'é', 'ñ', 'я', 'Ω', '€', '\u00a0', ' ', 'z', 'Z', '+', ',', '\\', 'F5', 'ArrowUp', 'Tab', 'Enter'];
		for (const key of keys) {
			for (const code of [undefined, 'KeyA', 'Digit1', 'Minus', 'Space']) {
				for (const alt of [false, true]) {
					const c = chordFromEvent({ ...ev(key, { alt, ctrl: true }), code });
					if (c) expect(parseChord(formatChord(c, 'other'), 'other')).toEqual(c);
				}
			}
		}
	});

	test('a letter of the alphabet a keyboard has is that letter, not the US key under it', () => {
		// ß sits over the US `-`; it must not become the zoom-out key.
		expect(chordFromEvent(ev('ß', {}, 'Minus'))).toBeNull();
		expect(matchAction(ev('ß', {}, 'Minus'), resolveBindings(emptyOverrides(), 'other'))).toBeNull();
		expect(chordFromEvent(ev('ö', {}, 'Semicolon'))).toEqual(chord('ö'));
		expect(chordFromEvent(ev('Ö', { shift: true }, 'Semicolon'))).toEqual(chord('Shift+ö'));
		// …but with Option held a Mac types a symbol or a Latin letter over the real key.
		expect(chordFromEvent(ev('ß', { alt: true }, 'KeyS'))).toEqual(chord('Alt+S'));
		expect(chordFromEvent(ev('ø', { alt: true }, 'KeyO'))).toEqual(chord('Alt+O'));
	});

	test('what cannot be a shortcut', () => {
		for (const k of ['Shift', 'Control', 'Alt', 'Meta', 'CapsLock', 'AltGraph']) expect(chordFromEvent(ev(k))).toBeNull();
		expect(chordFromEvent(ev('MediaPlayPause'))).toBeNull();
		expect(chordFromEvent(ev('Dead'))).toBeNull();
		expect(chordFromEvent(ev('Process'))).toBeNull();
		// AltGr is how a character is typed on many layouts, not a modifier.
		expect(chordFromEvent({ ...ev('@', { ctrl: true, alt: true }), getModifierState: (k) => k === 'AltGraph' })).toBeNull();
	});
});

describe('the registry', () => {
	test('ids are unique and every action says what it is', () => {
		const ids = ACTIONS.map((a) => a.id);
		expect(new Set(ids).size).toBe(ids.length);
		for (const a of ACTIONS) {
			expect(a.label.trim().length).toBeGreaterThan(0);
			expect(GROUPS.some((g) => g.id === a.group)).toBe(true);
		}
		for (const g of GROUPS) expect(ACTIONS.some((a) => a.group === g.id)).toBe(true);
	});

	test('every default is a chord, in its canonical spelling', () => {
		for (const p of PLATFORMS) {
			for (const a of ACTIONS) {
				for (const d of a.defaults) {
					const c = parseChord(d, p);
					expect(c).not.toBeNull();
					expect(formatChord(c!, p)).toBe(d.length === 1 ? d.toUpperCase() : d);
				}
			}
		}
	});

	test('no two actions share a default chord', () => {
		for (const p of PLATFORMS) {
			expect(findCollisions(resolveBindings(emptyOverrides(), p), ACTIONS)).toEqual([]);
			// …and resolving dropped nothing: the defaults were clean to begin with.
			const resolved = resolveBindings(emptyOverrides(), p);
			for (const a of ACTIONS) expect(resolved[a.id].length).toBe(a.defaults.length);
		}
	});

	test('the page handles every action it is given', () => {
		const page = readFileSync(join(import.meta.dir, '../routes/+page.svelte'), 'utf8');
		for (const a of ACTIONS) expect(page).toContain(`'${a.id}'`);
	});
});

// ---- the defaults are the shortcuts the editor always had ---------------------

/** The page's key handler as it was before the registry existed, reduced to
 *  "which action does this keypress run". Kept here verbatim-in-spirit so the
 *  defaults can be held to it. */
function legacyAction(e: KeyEventLike): string | null {
	const k = e.key.toLowerCase();
	if (e.metaKey || e.ctrlKey) {
		if (k === 'z') return e.shiftKey ? 'edit.redo' : 'edit.undo';
		if (k === 'y') return 'edit.redo';
		if (k === 's') return 'file.save';
		if (k === 'o') return 'file.open';
		if (k === 'n') return 'file.new';
		if (k === 'e') return 'file.export';
		if (k === 'i') return 'file.import';
		if (e.key === ',') return 'app.settings';
		if (k === 'a') return 'edit.selectAll';
		if (k === 'c') return 'edit.copy';
		if (k === 'x') return 'edit.cut';
		if (k === 'v') return 'edit.paste';
		if (k === 'd') return 'edit.duplicate';
		return null;
	}
	if (k === 'v') return 'tool.pointer';
	if (k === 'c') return 'tool.razor';
	if (k === 'r' && !e.shiftKey && !e.altKey) return 'tool.rippleMode';
	if (k === 'z' && e.shiftKey) return 'view.zoomFit';
	if (k === 'm') return 'marker.add';
	if (e.key === ',') return 'marker.prev';
	if (e.key === '.') return 'marker.next';
	if (k === 'j') return 'playback.shuttleBack';
	if (k === 'k') return 'playback.pause';
	if (k === 'l') return 'playback.shuttleForward';
	if (k === 'i') return e.shiftKey ? 'range.clearIn' : 'range.markIn';
	if (k === 'o') return e.shiftKey ? 'range.clearOut' : 'range.markOut';
	if (e.key === ' ') return 'playback.toggle';
	if (e.key === 'ArrowLeft') return e.shiftKey ? 'playback.jumpBack' : 'playback.stepBack';
	if (e.key === 'ArrowRight') return e.shiftKey ? 'playback.jumpForward' : 'playback.stepForward';
	if (e.key === 'Home') return 'playback.toStart';
	if (e.key === 'End') return 'playback.toEnd';
	if (e.key === '+' || e.key === '=') return 'view.zoomIn';
	if (e.key === '-') return 'view.zoomOut';
	if (e.key === 'Escape') return 'edit.clearSelection';
	if (e.key === 'Delete' || e.key === 'Backspace') return e.shiftKey ? 'edit.rippleDelete' : 'edit.delete';
	return null;
}

/** Keys that were given an action after the registry existed — the old handler had no
 *  answer for them, so a bare press is held to this instead of to nothing. Adding a
 *  default belongs here, with the action it is, so the decision is written down. */
const ADDED: Record<string, string> = {
	n: 'tool.roll',
	y: 'tool.slip',
	u: 'tool.slide',
	q: 'edit.trimStart',
	w: 'edit.trimEnd'
};

/** What a keypress should do now: the old handler's answer, plus the added bare keys. */
function expectedAction(e: KeyEventLike): string | null {
	const was = legacyAction(e);
	if (was) return was;
	if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return null;
	return ADDED[e.key.toLowerCase()] ?? null;
}

describe('the default keys are the ones the editor always had', () => {
	test('each shortcut, written out', () => {
		// ⌘ / Ctrl chords, then bare keys.
		const withMod: [string, Mods, string][] = [
			['z', {}, 'edit.undo'],
			['z', { shift: true }, 'edit.redo'],
			['Z', { shift: true }, 'edit.redo'],
			['y', {}, 'edit.redo'],
			['s', {}, 'file.save'],
			['s', { shift: true }, 'file.save'], // ⇧⌘S, the habit for "save as"
			['o', {}, 'file.open'],
			['n', {}, 'file.new'],
			['e', {}, 'file.export'],
			['i', {}, 'file.import'],
			[',', {}, 'app.settings'],
			['a', {}, 'edit.selectAll'],
			['c', {}, 'edit.copy'],
			['x', {}, 'edit.cut'],
			['v', {}, 'edit.paste'],
			['d', {}, 'edit.duplicate']
		];
		const bare: [string, Mods, string][] = [
			['v', {}, 'tool.pointer'],
			['c', {}, 'tool.razor'],
			['n', {}, 'tool.roll'],
			['y', {}, 'tool.slip'],
			['u', {}, 'tool.slide'],
			['q', {}, 'edit.trimStart'],
			['w', {}, 'edit.trimEnd'],
			['r', {}, 'tool.rippleMode'],
			['z', { shift: true }, 'view.zoomFit'],
			['m', {}, 'marker.add'],
			[',', {}, 'marker.prev'],
			['.', {}, 'marker.next'],
			['j', {}, 'playback.shuttleBack'],
			['k', {}, 'playback.pause'],
			['l', {}, 'playback.shuttleForward'],
			['i', {}, 'range.markIn'],
			['o', {}, 'range.markOut'],
			['i', { shift: true }, 'range.clearIn'],
			['o', { shift: true }, 'range.clearOut'],
			[' ', {}, 'playback.toggle'],
			['ArrowLeft', {}, 'playback.stepBack'],
			['ArrowRight', {}, 'playback.stepForward'],
			['ArrowLeft', { shift: true }, 'playback.jumpBack'],
			['ArrowRight', { shift: true }, 'playback.jumpForward'],
			['Home', {}, 'playback.toStart'],
			['End', {}, 'playback.toEnd'],
			['+', {}, 'view.zoomIn'],
			['+', { shift: true }, 'view.zoomIn'], // Shift+= on a US keyboard
			['=', {}, 'view.zoomIn'],
			['-', {}, 'view.zoomOut'],
			['Escape', {}, 'edit.clearSelection'],
			['Delete', {}, 'edit.delete'],
			['Backspace', {}, 'edit.delete'],
			['Delete', { shift: true }, 'edit.rippleDelete'],
			['Backspace', { shift: true }, 'edit.rippleDelete']
		];
		for (const p of PLATFORMS) {
			const b = resolveBindings(emptyOverrides(), p);
			for (const [key, m, id] of withMod) expect([p, key, matchAction(ev(key, { ...primary(p), ...m }), b)]).toEqual([p, key, id]);
			for (const [key, m, id] of bare) expect([p, key, matchAction(ev(key, m), b)]).toEqual([p, key, id]);
		}
	});

	test('the same key, with Caps Lock on, does the same thing', () => {
		for (const p of PLATFORMS) {
			const b = resolveBindings(emptyOverrides(), p);
			for (const k of 'vcrmjklionyuqw') expect(matchAction(ev(k.toUpperCase()), b)).toBe(matchAction(ev(k), b));
			expect(matchAction(ev('Z', { ...primary(p) }), b)).toBe('edit.undo');
		}
	});

	// Held against the old handler over every key it could see, with no modifier
	// and with the platform's primary one: nothing it did is lost or changed.
	const KEYS = [
		...'abcdefghijklmnopqrstuvwxyz0123456789',
		...'abcdefghijklmnopqrstuvwxyz'.toUpperCase(),
		...',.+-=[];\'/\\`',
		' ',
		'Enter',
		'Tab',
		'Escape',
		'Backspace',
		'Delete',
		'Home',
		'End',
		'PageUp',
		'PageDown',
		'ArrowLeft',
		'ArrowRight',
		'ArrowUp',
		'ArrowDown',
		'F5'
	];

	test('with no modifier, every key does what it did', () => {
		for (const p of PLATFORMS) {
			const b = resolveBindings(emptyOverrides(), p);
			for (const key of KEYS) expect([key, matchAction(ev(key), b)]).toEqual([key, expectedAction(ev(key))]);
		}
	});

	test('the keys added since are bare, so ⌘N is still a new project and ⇧Q does nothing', () => {
		for (const p of PLATFORMS) {
			const b = resolveBindings(emptyOverrides(), p);
			for (const k of Object.keys(ADDED)) {
				expect(matchAction(ev(k, { shift: true }), b)).toBeNull();
				expect(matchAction(ev(k, { alt: true }), b)).toBeNull();
			}
			expect(matchAction(ev('n', primary(p)), b)).toBe('file.new');
			for (const k of 'yuqw') expect(matchAction(ev(k, primary(p)), b)).toBe(k === 'y' ? 'edit.redo' : null);
		}
	});

	test('with ⌘ / Ctrl, every key does what it did', () => {
		for (const p of PLATFORMS) {
			const b = resolveBindings(emptyOverrides(), p);
			for (const key of KEYS) {
				const e = ev(key, primary(p));
				expect([key, matchAction(e, b)]).toEqual([key, legacyAction(e)]);
			}
		}
	});

	test('with Shift, every key does what it did — bar the accidents', () => {
		// The old handler ignored Shift on most keys, so ⇧J shuttled and ⇧Space
		// played. A chord now means exactly what it says; these are the keys that
		// used to answer to a Shift nobody asked for.
		const accidents = new Set(['v', 'c', 'm', 'j', 'k', 'l', ' ', 'Home', 'End', 'Escape'].flatMap((k) => [k, k.toUpperCase()]));
		for (const p of PLATFORMS) {
			const b = resolveBindings(emptyOverrides(), p);
			for (const key of KEYS) {
				const e = ev(key, { shift: true });
				const now = matchAction(e, b);
				if (accidents.has(key)) expect([key, now]).toEqual([key, null]);
				else expect([key, now]).toEqual([key, legacyAction(e)]);
			}
		}
	});

	test('what the old handler did by accident with ⇧⌘ is the one thing kept, for save', () => {
		for (const p of PLATFORMS) {
			const b = resolveBindings(emptyOverrides(), p);
			for (const key of KEYS) {
				const e = ev(key, { ...primary(p), shift: true });
				const now = matchAction(e, b);
				const was = legacyAction(e);
				if (now !== was) expect([key, now]).toEqual([key, null]);
			}
			expect(matchAction(ev('s', { ...primary(p), shift: true }), b)).toBe('file.save');
		}
	});

	test('the other platform’s modifier is not Mod', () => {
		const mac = resolveBindings(emptyOverrides(), 'mac');
		const other = resolveBindings(emptyOverrides(), 'other');
		expect(matchAction(ev('z', { meta: true }), mac)).toBe('edit.undo');
		expect(matchAction(ev('z', { ctrl: true }), mac)).toBeNull(); // ⌃Z on a Mac
		expect(matchAction(ev('z', { ctrl: true }), other)).toBe('edit.undo');
		expect(matchAction(ev('z', { meta: true }), other)).toBeNull(); // the Windows key
	});

	test('Alt on a chord that has none means a different chord', () => {
		const b = resolveBindings(emptyOverrides(), 'other');
		expect(matchAction(ev('r', { alt: true }), b)).toBeNull();
		expect(matchAction(ev('z', { ctrl: true, alt: true }), b)).toBeNull();
	});
});

// ---- resolving, matching, conflicts --------------------------------------------

const act = (id: string, defaults: string[], context: ActionDef['context'] = 'global'): ActionDef => ({
	id,
	label: id.toUpperCase(),
	group: 'tools',
	context,
	defaults
});

describe('what is in force', () => {
	const A = act('a', ['Q']);
	const B = act('b', ['W']);
	const C = act('c', ['E']);
	const actions = [A, B, C];
	const o = (bindings: Record<string, string[]>): KeyOverrides => ({ version: 1, bindings });

	test('untouched actions have their defaults; a customised one has exactly its own', () => {
		const r = resolveBindings(o({ b: ['X', 'Y'] }), 'other', actions);
		expect(r.a.map((c) => c.key)).toEqual(['q']);
		expect(r.b.map((c) => c.key)).toEqual(['x', 'y']);
		expect(r.c.map((c) => c.key)).toEqual(['e']);
	});

	test('a stored empty list is an action left unbound on purpose', () => {
		expect(resolveBindings(o({ a: [] }), 'other', actions).a).toEqual([]);
	});

	test('a customisation beats the default of an action the user did not touch', () => {
		// B was moved onto A's key: A loses it rather than both firing.
		const r = resolveBindings(o({ b: ['Q'] }), 'other', actions);
		expect(r.b.map((c) => c.key)).toEqual(['q']);
		expect(r.a).toEqual([]);
		expect(matchAction(ev('q'), r, actions)).toBe('b');
	});

	test('between two customisations the earlier action in the registry wins', () => {
		const r = resolveBindings(o({ a: ['X'], c: ['X'] }), 'other', actions);
		expect(r.a.map((c) => c.key)).toEqual(['x']);
		expect(r.c).toEqual([]);
		expect(findCollisions(r, actions)).toEqual([]);
	});

	test('a chord twice in one action is once', () => {
		expect(resolveBindings(o({ a: ['X', 'x'] }), 'other', actions).a).toHaveLength(1);
	});

	test('two contexts that can never both be live may share a chord', () => {
		const t = act('t', ['Q'], 'timeline');
		const p = act('p', ['Q'], 'preview');
		const g = act('g', ['Q'], 'global');
		const both = resolveBindings(emptyOverrides(), 'other', [t, p]);
		expect(both.t).toHaveLength(1);
		expect(both.p).toHaveLength(1);
		expect(matchAction(ev('q'), both, [t, p], ['timeline'])).toBe('t');
		expect(matchAction(ev('q'), both, [t, p], ['preview'])).toBe('p');
		expect(matchAction(ev('q'), both, [t, p], ['global'])).toBeNull();
		// A global action overlaps everything, so it competes with both.
		const withGlobal = resolveBindings(emptyOverrides(), 'other', [g, t]);
		expect(withGlobal.g).toHaveLength(1);
		expect(withGlobal.t).toHaveLength(0);
	});

	test('conflicts name the other actions that hold the chord', () => {
		const r = resolveBindings(emptyOverrides(), 'other', actions);
		expect(conflictsFor(r, 'a', chord('W'), actions).map((x) => x.id)).toEqual(['b']);
		expect(conflictsFor(r, 'a', chord('Q'), actions)).toEqual([]); // its own is not a conflict
		expect(conflictsFor(r, 'a', chord('Z'), actions)).toEqual([]);
		expect(conflictsFor(r, 'nope', chord('W'), actions)).toEqual([]);
	});

	test('an event matches nothing when nothing is bound to it, or it is only a modifier', () => {
		const r = resolveBindings(emptyOverrides(), 'other', actions);
		expect(matchAction(ev('z'), r, actions)).toBeNull();
		expect(matchAction(ev('Shift', { shift: true }), r, actions)).toBeNull();
		expect(matchAction(ev('q'), r, actions)).toBe('a');
	});
});

// ---- reading what was stored, and carrying it across versions -----------------

describe('reading stored overrides', () => {
	const A = act('a', ['Q']);
	const B = act('b', ['W', 'Shift+W']);
	const actions = [A, B];
	const read = (raw: unknown, platform: Platform = 'other') => parseKeyOverrides(raw, platform, { actions, version: 1 });

	test('nothing, or nonsense, is no customisation', () => {
		for (const raw of [null, undefined, 3, 'x', [], {}, { version: 1 }, { bindings: 5 }, { version: 1, bindings: [1] }]) {
			expect(read(raw)).toEqual({ version: 1, bindings: {} });
		}
	});

	test('what is stored comes back, spelled canonically', () => {
		expect(read({ version: 1, bindings: { a: ['ctrl+x', 'shift+f5'] } }).bindings).toEqual({ a: ['Mod+X', 'Shift+F5'] });
	});

	test('actions that no longer exist are dropped', () => {
		expect(read({ version: 1, bindings: { a: ['X'], gone: ['Y'] } }).bindings).toEqual({ a: ['X'] });
	});

	test('chords that are not chords are dropped; an action left with none reverts to its defaults', () => {
		expect(read({ version: 1, bindings: { a: ['X', 'banana'], b: ['banana', 7, null] } }).bindings).toEqual({ a: ['X'] });
	});

	test('an empty list is a decision to unbind, and is kept', () => {
		expect(read({ version: 1, bindings: { a: [] } }).bindings).toEqual({ a: [] });
	});

	test('what equals the defaults is not an override', () => {
		expect(read({ version: 1, bindings: { a: ['q'], b: ['W', 'Shift+W'] } }).bindings).toEqual({});
		// …but the same chords in another order are the user's order.
		expect(read({ version: 1, bindings: { b: ['Shift+W', 'W'] } }).bindings).toEqual({ b: ['Shift+W', 'W'] });
	});

	test('a chord listed twice is kept once', () => {
		expect(read({ version: 1, bindings: { a: ['X', 'x'] } }).bindings).toEqual({ a: ['X'] });
	});

	test('reading what was written is the identity', () => {
		const once = read({ version: 1, bindings: { a: ['ctrl+x'], b: [] } });
		expect(read(JSON.parse(JSON.stringify(serializeOverrides(once))))).toEqual(once);
	});

	test('a file with no version is read as the first', () => {
		const step: Migration = { to: 2, apply: (b) => ({ ...b, a: b.old ?? b.a }) };
		const r = parseKeyOverrides({ bindings: { old: ['X'] } }, 'other', { actions, version: 2, migrations: [step] });
		expect(r).toEqual({ version: 2, bindings: { a: ['X'] } });
	});

	test('nothing customised is stored as nothing', () => {
		expect(serializeOverrides(emptyOverrides())).toBeNull();
		expect(serializeOverrides({ version: 1, bindings: { a: [] } })).toEqual({ version: 1, bindings: { a: [] } });
	});

	test('the shipped registry reads its own defaults back as no customisation', () => {
		for (const p of PLATFORMS) {
			const all = Object.fromEntries(ACTIONS.map((a) => [a.id, [...a.defaults]]));
			expect(parseKeyOverrides({ version: KEYMAP_VERSION, bindings: all }, p).bindings).toEqual({});
		}
	});
});

describe('defaults changing between versions', () => {
	// The registry as version 1 shipped it, and as version 2 changed it: Q moved
	// to Z, W was split into two actions, and a new action took a key that a
	// user might already be using.
	const v1: ActionDef[] = [act('quit', ['Q']), act('wipe', ['W']), act('save', ['S'])];
	const v2: ActionDef[] = [act('quit', ['Z']), act('wipe.clip', ['W']), act('wipe.all', ['Shift+W']), act('save', ['S']), act('fresh', ['X'])];
	const toV2: Migration = {
		to: 2,
		apply: (b) => {
			const { wipe, ...rest } = b;
			// The old action became the clip one; the new one is not customised.
			return wipe ? { ...rest, 'wipe.clip': wipe } : rest;
		}
	};
	const read = (raw: unknown) => parseKeyOverrides(raw, 'other', { actions: v2, version: 2, migrations: [toV2] });
	const keys = (r: ReturnType<typeof resolveBindings>, id: string) => r[id].map((c) => formatChord(c, 'other'));

	test('an untouched action follows the new default', () => {
		const stored = { version: 1, bindings: { save: ['F2'] } }; // only `save` was customised
		const o = read(stored);
		const r = resolveBindings(o, 'other', v2);
		expect(keys(r, 'quit')).toEqual(['Z']);
		expect(keys(r, 'save')).toEqual(['F2']); // …and the customised one is kept
	});

	test('a customised action keeps its choice when its default moved', () => {
		const o = read({ version: 1, bindings: { quit: ['Q'] } });
		// Q is no longer quit's default, so this is now a real customisation.
		expect(o.bindings).toEqual({ quit: ['Q'] });
		expect(keys(resolveBindings(o, 'other', v2), 'quit')).toEqual(['Q']);
	});

	test('a customisation equal to the new default stops being one', () => {
		const o = read({ version: 1, bindings: { quit: ['Z'] } });
		expect(o.bindings).toEqual({});
	});

	test('a migration carries a renamed action’s customisation to its new id', () => {
		const o = read({ version: 1, bindings: { wipe: ['F9'] } });
		expect(o.bindings).toEqual({ 'wipe.clip': ['F9'] });
		const r = resolveBindings(o, 'other', v2);
		expect(keys(r, 'wipe.clip')).toEqual(['F9']);
		expect(keys(r, 'wipe.all')).toEqual(['Shift+W']);
	});

	test('a file already at the new version is not migrated twice', () => {
		const o = read({ version: 2, bindings: { wipe: ['F9'], 'wipe.clip': ['F8'] } });
		expect(o.bindings).toEqual({ 'wipe.clip': ['F8'] }); // `wipe` is unknown at v2 and dropped
	});

	test('a key the user already uses is not taken by a default added later', () => {
		// They put `save` on X; v2 gives the new `fresh` action X as its default.
		const o = read({ version: 1, bindings: { save: ['X'] } });
		const r = resolveBindings(o, 'other', v2);
		expect(keys(r, 'save')).toEqual(['X']);
		expect(keys(r, 'fresh')).toEqual([]);
		expect(findCollisions(r, v2)).toEqual([]);
	});

	test('an action left unbound on purpose stays unbound', () => {
		const o = read({ version: 1, bindings: { quit: [] } });
		expect(keys(resolveBindings(o, 'other', v2), 'quit')).toEqual([]);
	});

	test('a file from a newer build keeps what this one knows', () => {
		const o = read({ version: 9, bindings: { save: ['F2'], 'from.the.future': ['F3'] } });
		expect(o).toEqual({ version: 2, bindings: { save: ['F2'] } });
	});

	test('steps run in order, from the version the file was written at', () => {
		const log: number[] = [];
		const step = (to: number): Migration => ({ to, apply: (b) => (log.push(to), b) });
		const run = (version: number) => {
			log.length = 0;
			parseKeyOverrides({ version, bindings: {} }, 'other', { actions: v2, version: 4, migrations: [step(4), step(2), step(3)] });
			return [...log];
		};
		expect(run(1)).toEqual([2, 3, 4]);
		expect(run(2)).toEqual([3, 4]);
		expect(run(4)).toEqual([]);
	});
});

// ---- editing ---------------------------------------------------------------------

describe('editing bindings', () => {
	const A = act('a', ['Q', 'Shift+Q']);
	const B = act('b', ['W']);
	const C = act('c', ['E']);
	const actions = [A, B, C];
	const platform: Platform = 'other';
	const resolve = (o: KeyOverrides) => resolveBindings(o, platform, actions);
	const set = (o: KeyOverrides, id: string, ...chords: string[]) =>
		setActionChords(o, id, chords.map((c) => chord(c)), platform, actions);
	const keysOf = (o: KeyOverrides, id: string) => resolve(o)[id].map((c) => formatChord(c, platform));

	test('setting an action’s chords records them', () => {
		expect(set(emptyOverrides(), 'b', 'X', 'Y').bindings).toEqual({ b: ['X', 'Y'] });
	});

	test('setting them back to the defaults forgets the override', () => {
		const o = set(emptyOverrides(), 'b', 'X');
		expect(set(o, 'b', 'W').bindings).toEqual({});
		expect(set(emptyOverrides(), 'a', 'Q', 'Shift+Q').bindings).toEqual({});
	});

	test('no chords at all is an unbound action, not a reset', () => {
		expect(set(emptyOverrides(), 'b').bindings).toEqual({ b: [] });
	});

	test('an unknown action is ignored', () => {
		const o = emptyOverrides();
		expect(set(o, 'nope', 'X')).toBe(o);
	});

	test('reset puts one action, or all of them, back', () => {
		const o = set(set(emptyOverrides(), 'a', 'X'), 'b', 'Y');
		expect(resetAction(o, 'a').bindings).toEqual({ b: ['Y'] });
		expect(resetAction(o, 'c')).toBe(o);
		expect(resetAll(o).bindings).toEqual({});
	});

	test('a chord that nothing uses goes straight in — replacing one, or added beside them', () => {
		const o = emptyOverrides();
		const b = resolve(o);
		const replace = { id: 'a', chord: chord('X'), replacing: chord('Q') };
		expect(rebindConflicts(b, replace, actions)).toEqual([]);
		expect(keysOf(applyRebind(o, b, replace, null, platform, actions), 'a')).toEqual(['X', 'Shift+Q']);
		const add = { id: 'b', chord: chord('X'), replacing: null };
		expect(keysOf(applyRebind(o, b, add, null, platform, actions), 'b')).toEqual(['W', 'X']);
	});

	test('a chord the action already has is not listed twice', () => {
		const o = emptyOverrides();
		const r = applyRebind(o, resolve(o), { id: 'a', chord: chord('Shift+Q'), replacing: chord('Q') }, null, platform, actions);
		expect(keysOf(r, 'a')).toEqual(['Shift+Q']);
	});

	test('a collision can be settled by taking the chord from the other action', () => {
		const o = emptyOverrides();
		const req = { id: 'a', chord: chord('W'), replacing: chord('Q') };
		expect(rebindConflicts(resolve(o), req, actions).map((x) => x.id)).toEqual(['b']);
		const r = applyRebind(o, resolve(o), req, 'unbind', platform, actions);
		expect(keysOf(r, 'a')).toEqual(['W', 'Shift+Q']);
		expect(keysOf(r, 'b')).toEqual([]);
		expect(findCollisions(resolve(r), actions)).toEqual([]);
	});

	test('…or by swapping: the other action gets what this one gave up', () => {
		const o = emptyOverrides();
		const req = { id: 'a', chord: chord('W'), replacing: chord('Q') };
		const r = applyRebind(o, resolve(o), req, 'swap', platform, actions);
		expect(keysOf(r, 'a')).toEqual(['W', 'Shift+Q']);
		expect(keysOf(r, 'b')).toEqual(['Q']);
		expect(findCollisions(resolve(r), actions)).toEqual([]);
	});

	test('swapping two actions’ only chords puts each on the other’s, and leaves nothing customised if that is the defaults', () => {
		const o = emptyOverrides();
		const r1 = applyRebind(o, resolve(o), { id: 'b', chord: chord('E'), replacing: chord('W') }, 'swap', platform, actions);
		expect(keysOf(r1, 'b')).toEqual(['E']);
		expect(keysOf(r1, 'c')).toEqual(['W']);
		// And swapping them back is no customisation at all.
		const r2 = applyRebind(r1, resolve(r1), { id: 'b', chord: chord('W'), replacing: chord('E') }, 'swap', platform, actions);
		expect(r2.bindings).toEqual({});
	});

	test('with nothing to give up, a swap is an unbind', () => {
		const o = emptyOverrides();
		const req = { id: 'a', chord: chord('W'), replacing: null };
		const r = applyRebind(o, resolve(o), req, 'swap', platform, actions);
		expect(keysOf(r, 'a')).toEqual(['Q', 'Shift+Q', 'W']);
		expect(keysOf(r, 'b')).toEqual([]);
	});

	test('a collision that is not settled changes nothing', () => {
		const o = emptyOverrides();
		const r = applyRebind(o, resolve(o), { id: 'a', chord: chord('W'), replacing: null }, null, platform, actions);
		expect(r).toBe(o);
	});
});

// ---- putting an action back ------------------------------------------------------------

describe('resetting an action', () => {
	const A = act('a', ['Q', 'Shift+Q']);
	const B = act('b', ['W']);
	const C = act('c', ['E']);
	const actions = [A, B, C];
	const platform: Platform = 'other';
	const resolve = (o: KeyOverrides) => resolveBindings(o, platform, actions);
	const keysOf = (o: KeyOverrides, id: string) => resolve(o)[id].map((c) => formatChord(c, platform));
	const o = (bindings: Record<string, string[]>): KeyOverrides => ({ version: 1, bindings });
	const reset = (ov: KeyOverrides, id: string, how: 'swap' | 'unbind' | null = null) =>
		applyReset(ov, resolve(ov), id, how, platform, actions);

	test('an action differs from its defaults when its chords do — not only when it has an override', () => {
		const b = (ov: KeyOverrides, id: string) => differsFromDefault(resolve(ov), id, platform, actions);
		expect(b(o({}), 'a')).toBe(false);
		expect(b(o({ a: ['X'] }), 'a')).toBe(true);
		expect(b(o({ a: [] }), 'a')).toBe(true);
		expect(b(o({ a: ['Shift+Q', 'Q'] }), 'a')).toBe(true); // the same chords, reordered
		// B was given A's first default: A has no override, and still differs.
		const taken = o({ b: ['Q'] });
		expect(taken.bindings.a).toBeUndefined();
		expect(b(taken, 'a')).toBe(true);
		expect(b(taken, 'c')).toBe(false);
		expect(differsFromDefault(resolve(o({})), 'nope', platform, actions)).toBe(false);
	});

	test('with nothing in the way, reset brings the defaults back', () => {
		const ov = o({ a: ['X'], b: [] });
		expect(resetPlan(resolve(ov), 'a', platform, actions)).toEqual({ conflicts: [], chord: null, gives: chord('X') });
		const r = reset(ov, 'a');
		expect(keysOf(r, 'a')).toEqual(['Q', 'Shift+Q']);
		expect(r.bindings).toEqual({ b: [] });
		expect(differsFromDefault(resolve(r), 'a', platform, actions)).toBe(false);
	});

	test('a default that another action has been given is a collision, and nothing happens until it is settled', () => {
		const ov = o({ a: ['X'], b: ['Q'] }); // B took A's Q while A was elsewhere
		const plan = resetPlan(resolve(ov), 'a', platform, actions);
		expect(plan.conflicts.map((x) => x.id)).toEqual(['b']);
		expect(plan.chord).toEqual(chord('Q'));
		expect(plan.gives).toEqual(chord('X'));
		expect(reset(ov, 'a')).toBe(ov);
	});

	test('…unbind takes the chord from the other action', () => {
		const ov = o({ a: ['X'], b: ['Q'] });
		const r = reset(ov, 'a', 'unbind');
		expect(keysOf(r, 'a')).toEqual(['Q', 'Shift+Q']);
		expect(keysOf(r, 'b')).toEqual([]);
		expect(findCollisions(resolve(r), actions)).toEqual([]);
	});

	test('…swap gives the other action what this one is giving up', () => {
		const ov = o({ a: ['X'], b: ['Q'] });
		const r = reset(ov, 'a', 'swap');
		expect(keysOf(r, 'a')).toEqual(['Q', 'Shift+Q']);
		expect(keysOf(r, 'b')).toEqual(['X']);
		expect(findCollisions(resolve(r), actions)).toEqual([]);
	});

	test('a swap with nothing to hand over is an unbind', () => {
		// A is only missing its Q (B has it); there is nothing of A's to give B.
		const ov = o({ b: ['Q'] });
		expect(resetPlan(resolve(ov), 'a', platform, actions).gives).toBeNull();
		const r = reset(ov, 'a', 'swap');
		expect(keysOf(r, 'a')).toEqual(['Q', 'Shift+Q']);
		expect(keysOf(r, 'b')).toEqual([]);
	});

	test('swapping two actions back to their defaults leaves nothing customised', () => {
		const ov = o({ b: ['E'], c: ['W'] }); // B and C traded keys
		const r = reset(ov, 'b', 'swap');
		expect(r.bindings).toEqual({});
	});

	test('an action left unbound on purpose is brought back too', () => {
		const ov = o({ b: [] });
		expect(keysOf(reset(ov, 'b'), 'b')).toEqual(['W']);
		expect(reset(ov, 'b').bindings).toEqual({});
	});

	test('an unknown action is left alone', () => {
		const ov = o({ a: ['X'] });
		expect(reset(ov, 'nope')).toBe(ov);
		expect(resetPlan(resolve(ov), 'nope', platform, actions).conflicts).toEqual([]);
	});
});

describe('keys that act once per press', () => {
	test('the actions that must not auto-repeat are exactly these', () => {
		const once = ACTIONS.filter((a) => !allowsRepeat(a.id)).map((a) => a.id);
		// Adding or removing one is a decision about what a held key does.
		expect(once).toEqual([
			'file.new',
			'file.open',
			'file.save',
			'file.import',
			'file.export',
			'app.settings',
			'edit.copy',
			'edit.cut',
			'edit.paste',
			'edit.duplicate',
			'edit.delete',
			'edit.rippleDelete',
			'edit.trimStart',
			'edit.trimEnd',
			'tool.rippleMode',
			'playback.toggle',
			'playback.shuttleBack',
			'playback.shuttleForward',
			'marker.add'
		]);
	});

	test('stepping, zooming and undoing keep repeating, and so does an action nobody has heard of', () => {
		for (const id of ['playback.stepBack', 'playback.stepForward', 'playback.jumpBack', 'playback.jumpForward', 'view.zoomIn', 'view.zoomOut', 'edit.undo', 'edit.redo', 'marker.next']) {
			expect(allowsRepeat(id)).toBe(true);
		}
		expect(allowsRepeat('nope')).toBe(true);
	});
});

// ---- the list in the settings dialog ----------------------------------------------

describe('searching the list', () => {
	const b = resolveBindings(emptyOverrides(), 'other');
	const ids = (q: string, p: Platform = 'other') => filterActions(q, resolveBindings(emptyOverrides(), p), p).map((a) => a.id);

	test('an empty search is everything', () => {
		expect(filterActions('', b, 'other')).toHaveLength(ACTIONS.length);
		expect(filterActions('   ', b, 'other')).toHaveLength(ACTIONS.length);
	});

	test('finds by what the action is called', () => {
		// (the two trims say in their hint that they follow ripple mode, and a hint is searched)
		expect(ids('ripple')).toEqual(['edit.rippleDelete', 'edit.trimStart', 'edit.trimEnd', 'tool.rippleMode']);
		expect(ids('shuttle')).toEqual(['playback.shuttleBack', 'playback.shuttleForward']);
	});

	test('finds by group, id and the chord as it is shown or stored', () => {
		expect(ids('markers')).toContain('marker.add');
		expect(ids('file.')).toContain('file.save');
		expect(ids('ctrl+z')).toContain('edit.undo');
		expect(ids('mod+shift+z')).toContain('edit.redo');
		expect(ids('⌘z', 'mac')).toContain('edit.undo');
		expect(ids('space')).toContain('playback.toggle');
	});

	test('every word has to match', () => {
		expect(ids('zoom fit')).toEqual(['view.zoomFit']);
		expect(ids('zoom banana')).toEqual([]);
	});

	test('a search follows a rebinding', () => {
		const o = setActionChords(emptyOverrides(), 'playback.toggle', [chord('P')], 'other');
		const r = resolveBindings(o, 'other');
		expect(filterActions('P', r, 'other').map((a) => a.id)).toContain('playback.toggle');
		expect(filterActions('space', r, 'other').map((a) => a.id)).not.toContain('playback.toggle');
	});
});

// ---- the registry is the only place that knows a key --------------------------------

function sources(dir: string): string[] {
	return readdirSync(dir).flatMap((name) => {
		const path = join(dir, name);
		if (statSync(path).isDirectory()) return sources(path);
		return /\.(ts|svelte)$/.test(name) && !name.endsWith('.test.ts') ? [path] : [];
	});
}

describe('no key is hard-coded outside the registry', () => {
	const SRC = join(import.meta.dir, '..');
	const files = sources(SRC).filter((f) => !f.endsWith('keymap.ts'));

	test('menus do not spell a shortcut', () => {
		const offenders = files.filter((f) => /\bshortcut:\s*['"`](?!\$\{)/.test(readFileSync(f, 'utf8')));
		expect(offenders).toEqual([]);
	});

	test('tooltips and labels do not spell one either', () => {
		// A modifier glyph, or a `(R)` / `(Space)` hint inside a string, in code
		// that is not a comment is a key being printed by hand. (A pointer gesture
		// — wheel, click — is not a binding and may say ⌘.)
		const printed = /[⌘⇧⌥⌃]|['"`][^'"`\n]*\((?:[A-Z]|Space|Del|Esc|Enter)\)/;
		const offenders = files.flatMap((f) =>
			readFileSync(f, 'utf8')
				.split('\n')
				.map((line, i) => ({ line, at: `${f.slice(SRC.length + 1)}:${i + 1}` }))
				.filter(({ line }) => !/^\s*(\/\/|\*|\/\*|<!--)/.test(line) && printed.test(line) && !/wheel|click/i.test(line))
				.map(({ at }) => at)
		);
		expect(offenders).toEqual([]);
	});

	test('only the page’s dispatcher and the recorder read the keyboard for a shortcut', () => {
		const handlers = files
			.filter((f) => /onkeydown|keydown/.test(readFileSync(f, 'utf8')))
			.map((f) => f.slice(SRC.length + 1))
			.sort();
		// Widgets with their own keys (a tab rail, a dialog's Escape, a drag's
		// Escape, a text field's Enter, a slider's arrows) are not shortcuts and are
		// listed so a new one is a decision.
		expect(handlers).toEqual([
			'lib/components/editor/AgentPanel.svelte',
			'lib/components/editor/ContextMenu.svelte',
			'lib/components/editor/ExportDialog.svelte',
			'lib/components/editor/KeyboardSettings.svelte',
			'lib/components/editor/LibraryPanel.svelte',
			'lib/components/editor/MediaBin.svelte',
			'lib/components/editor/MixSlider.svelte',
			'lib/components/editor/NotificationCenter.svelte',
			'lib/components/editor/Preview.svelte',
			'lib/components/editor/SettingsDialog.svelte',
			'lib/components/editor/Timeline.svelte',
			'lib/components/editor/TitlesControls.svelte',
			'lib/components/editor/UpdateDialog.svelte',
			'lib/components/editor/VoiceoverDialog.svelte',
			'lib/components/editor/WorkspaceTabs.svelte',
			'lib/drag.ts',
			'lib/modal.ts',
			'routes/+page.svelte'
		]);
	});
});

test('sameChord compares all five parts', () => {
	expect(sameChord(chord('Mod+Z'), chord('Mod+Z'))).toBe(true);
	expect(sameChord(chord('Mod+Z'), chord('Mod+Shift+Z'))).toBe(false);
	expect(sameChord(chord('Mod+Z'), chord('Z'))).toBe(false);
	expect(sameChord(chord('Alt+Z'), chord('Z'))).toBe(false);
	expect(sameChord(chord('Mod+Z'), chord('Mod+Y'))).toBe(false);
});
