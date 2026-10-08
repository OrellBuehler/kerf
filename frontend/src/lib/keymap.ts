// Keyboard shortcuts: the action registry and everything pure about it.
//
// Every shortcut the editor handles is a named *action* here — an id, a label, a
// group for the settings list and one or more default chords. The page's key
// handler asks `matchAction` which action an event is and runs the handler it
// keeps for that id; menus and tooltips ask `displayChord` what to print. So a
// rebound key moves the behaviour, the menu hint and the tooltip together,
// because there is no second place that knows the key.
//
// What is stored is only what the user changed (`KeyOverrides`): an action they
// never touched is not in the file and follows whatever the defaults are in the
// build that is running. The file carries the registry `version` it was written
// against, so a later build that renames or splits an action can carry a
// customisation across (`MIGRATIONS`) instead of silently dropping it.
//
// ## Chords
//
// A chord is a key plus the four modifiers, matched *physically*: `ctrl` is the
// Control key and `meta` is ⌘ / the Windows key. The portable spelling stored in
// the settings file says `Mod` for the platform's primary modifier — ⌘ on macOS,
// Ctrl elsewhere — and `parseChord` resolves it, so a default of `Mod+Z` is ⌘Z
// on a Mac and Ctrl+Z everywhere else and the two never both fire. (The previous
// handler accepted either on every platform; a Ctrl chord on macOS or a ⌘ chord
// on Windows is now its own chord, which is what lets them be bound separately.)
//
// The *key* is what the keypress types, not where the key sits (`KeyboardEvent.key`,
// not `.code`): on AZERTY or Dvorak the Z shortcut is on the key that prints Z.
// Two exceptions, both because `key` stops being useful: a character outside
// ASCII (a Cyrillic or Greek layout, a macOS Option combination) falls back to
// the physical key's US-layout letter so ⌘Z still undoes on a Russian keyboard,
// and Shift is dropped from punctuation, because the character already says it —
// `+` is Shift+= on a US keyboard and a bare key on a German one, and a chord
// that said `Shift++` would only ever work on one of them.

export type Platform = 'mac' | 'other';

/** `navigator` as far as platform detection looks at it (`userAgentData` is not
 *  in every TypeScript DOM lib). */
export interface NavigatorLike {
	platform?: string;
	userAgent?: string;
	userAgentData?: { platform?: string };
}

export function detectPlatform(nav: NavigatorLike | undefined = typeof navigator === 'undefined' ? undefined : navigator): Platform {
	const p = nav?.userAgentData?.platform || nav?.platform || nav?.userAgent || '';
	return /mac|iphone|ipad|ipod/i.test(p) ? 'mac' : 'other';
}

// ---- chords ------------------------------------------------------------------

export interface Chord {
	/** A lowercase character (`z`, `,`, `+`) or a named key (`Space`, `ArrowLeft`, `F5`). */
	key: string;
	ctrl: boolean;
	meta: boolean;
	alt: boolean;
	shift: boolean;
}

/** The slice of a `KeyboardEvent` a chord is read from. */
export interface KeyEventLike {
	key: string;
	code?: string;
	ctrlKey: boolean;
	metaKey: boolean;
	altKey: boolean;
	shiftKey: boolean;
	getModifierState?: (key: string) => boolean;
}

/** Keys that can be bound by name, spelled as `KeyboardEvent.key` spells them. */
const NAMED_KEYS = [
	'Space',
	'Enter',
	'Tab',
	'Backspace',
	'Delete',
	'Escape',
	'Home',
	'End',
	'PageUp',
	'PageDown',
	'Insert',
	'ArrowLeft',
	'ArrowRight',
	'ArrowUp',
	'ArrowDown'
] as const;

/** Other spellings a hand-written chord may use, lowercase. */
const KEY_ALIASES: Record<string, string> = {
	spacebar: 'Space',
	return: 'Enter',
	esc: 'Escape',
	del: 'Delete',
	ins: 'Insert',
	pgup: 'PageUp',
	pgdn: 'PageDown',
	left: 'ArrowLeft',
	right: 'ArrowRight',
	up: 'ArrowUp',
	down: 'ArrowDown'
};

const NAMED_LOOKUP = new Map<string, string>([
	...NAMED_KEYS.map((k) => [k.toLowerCase(), k] as const),
	...Object.entries(KEY_ALIASES)
]);

/** What a key prints as in a menu. */
const KEY_GLYPHS: Record<string, string> = {
	ArrowLeft: '←',
	ArrowRight: '→',
	ArrowUp: '↑',
	ArrowDown: '↓',
	Escape: 'Esc',
	Delete: 'Del',
	PageUp: 'PgUp',
	PageDown: 'PgDn'
};
const MAC_KEY_GLYPHS: Record<string, string> = { Backspace: '⌫', Enter: '↵', Tab: '⇥' };

const MODIFIER_KEYS = new Set(['Control', 'Shift', 'Alt', 'Meta', 'AltGraph', 'OS', 'Super', 'Hyper', 'CapsLock', 'NumLock', 'ScrollLock', 'Fn', 'FnLock', 'Symbol', 'SymbolLock']);

const CODE_CHARS: Record<string, string> = {
	Minus: '-',
	Equal: '=',
	BracketLeft: '[',
	BracketRight: ']',
	Backslash: '\\',
	Semicolon: ';',
	Quote: "'",
	Backquote: '`',
	Comma: ',',
	Period: '.',
	Slash: '/',
	NumpadAdd: '+',
	NumpadSubtract: '-',
	NumpadMultiply: '*',
	NumpadDivide: '/',
	NumpadDecimal: '.'
};

/** The US-layout character of a physical key, for when `key` is unusable. */
function charFromCode(code: string | undefined): string | null {
	if (!code) return null;
	const m = /^(?:Key([A-Z])|Digit([0-9]))$/.exec(code);
	if (m) return (m[1] ?? m[2]).toLowerCase();
	return CODE_CHARS[code] ?? null;
}

const isAlnum = (ch: string) => /^[a-z0-9]$/.test(ch);
const isFunctionKey = (k: string) => /^F([1-9]|1[0-9]|2[0-4])$/.test(k);

const isLatinLetter = (ch: string) => /^\p{Script=Latin}$/u.test(ch);

/** The chord a keypress is, or null for one that cannot be a shortcut: a modifier
 *  on its own, a dead key with nothing under it, a media key, an AltGr character
 *  (typing, not a shortcut) — or anything that would not survive being written
 *  down and read back (`ß`, which upper-cases to `SS`; a stored chord that cannot
 *  be read is a shortcut that silently vanishes on the next launch). */
export function chordFromEvent(e: KeyEventLike): Chord | null {
	if (e.getModifierState?.('AltGraph')) return null;
	let key = e.key;
	if (MODIFIER_KEYS.has(key)) return null;
	let shift = e.shiftKey;

	if (/^\s$/u.test(key)) {
		// A space — or the no-break space macOS types for ⌥Space.
		key = 'Space';
	} else if (key === 'Dead' || key === 'Unidentified') {
		const c = charFromCode(e.code);
		if (!c) return null;
		key = c;
	} else if ([...key].length === 1) {
		if (/^[\x21-\x7e]$/.test(key)) {
			key = key.toLowerCase();
			// The character already carries the Shift: `+`, `?`, `<`.
			if (!isAlnum(key)) shift = false;
		} else if (!e.altKey && isLatinLetter(key)) {
			// `ö` on a German keyboard is its own key, not the US `;` it happens to
			// sit over (which would also make `ß` the zoom-out key).
			key = key.toLowerCase();
		} else {
			// A Cyrillic letter, a macOS Option character: take the key's US-layout
			// letter instead. Without a code there is nothing better than the character.
			key = charFromCode(e.code) ?? key.toLowerCase();
		}
	} else if (isFunctionKey(key) || (NAMED_KEYS as readonly string[]).includes(key)) {
		// as is
	} else {
		return null;
	}
	const chord: Chord = { key, ctrl: e.ctrlKey, meta: e.metaKey, alt: e.altKey, shift };
	// Whether the key is writable does not depend on the platform.
	const back = parseChord(formatChord(chord, 'other'), 'other');
	return back && sameChord(back, chord) ? chord : null;
}

export function sameChord(a: Chord, b: Chord): boolean {
	return a.key === b.key && a.ctrl === b.ctrl && a.meta === b.meta && a.alt === b.alt && a.shift === b.shift;
}

const MODIFIER_NAMES: Record<string, 'mod' | 'ctrl' | 'meta' | 'alt' | 'shift'> = {
	mod: 'mod',
	ctrl: 'ctrl',
	control: 'ctrl',
	cmd: 'meta',
	command: 'meta',
	meta: 'meta',
	super: 'meta',
	win: 'meta',
	alt: 'alt',
	option: 'alt',
	opt: 'alt',
	shift: 'shift'
};

/** Read a chord from its portable spelling (`Mod+Shift+Z`, `Shift+ArrowLeft`,
 *  `Mod+,`, `+`). Case-insensitive; null when it is not one. */
export function parseChord(text: string, platform: Platform): Chord | null {
	let rest = text.trim();
	const flags = { ctrl: false, meta: false, alt: false, shift: false };
	for (;;) {
		// A modifier name, then a `+`, then *something* — so `Mod++` is Mod and the
		// key `+`, and a bare `Shift+` is not a chord.
		const m = /^([A-Za-z]+)\+(?=[\s\S])/.exec(rest);
		const kind = m ? MODIFIER_NAMES[m[1].toLowerCase()] : undefined;
		if (!m || !kind) break;
		if (kind === 'mod') flags[platform === 'mac' ? 'meta' : 'ctrl'] = true;
		else flags[kind] = true;
		rest = rest.slice(m[0].length);
	}
	const key = parseKey(rest);
	return key ? { key, ...flags } : null;
}

function parseKey(text: string): string | null {
	if (!text) return null;
	const chars = [...text];
	if (chars.length === 1) {
		// Any one visible character; whitespace is `Space`.
		if (/\s/.test(text) || /[\x00-\x1f\x7f]/.test(text)) return null;
		return text.toLowerCase();
	}
	const named = NAMED_LOOKUP.get(text.toLowerCase());
	if (named) return named;
	const f = text.toUpperCase();
	return isFunctionKey(f) ? f : null;
}

const keyName = (key: string) => (key.length === 1 ? key.toUpperCase() : key);

/** The portable spelling that is stored: `Mod+Shift+Z`. Modifiers in a fixed
 *  order so equal chords have equal text. */
export function formatChord(c: Chord, platform: Platform): string {
	const primary = platform === 'mac' ? c.meta : c.ctrl;
	const other = platform === 'mac' ? c.ctrl : c.meta;
	const parts: string[] = [];
	if (primary) parts.push('Mod');
	if (other) parts.push(platform === 'mac' ? 'Ctrl' : 'Meta');
	if (c.alt) parts.push('Alt');
	if (c.shift) parts.push('Shift');
	parts.push(keyName(c.key));
	return parts.join('+');
}

/** How a chord reads in a menu or tooltip: `⇧⌘Z` on a Mac, `Ctrl+Shift+Z` elsewhere. */
export function displayChord(c: Chord, platform: Platform): string {
	const key = (platform === 'mac' ? MAC_KEY_GLYPHS[c.key] : undefined) ?? KEY_GLYPHS[c.key] ?? keyName(c.key);
	if (platform === 'mac') return `${c.ctrl ? '⌃' : ''}${c.alt ? '⌥' : ''}${c.shift ? '⇧' : ''}${c.meta ? '⌘' : ''}${key}`;
	return [c.ctrl && 'Ctrl', c.alt && 'Alt', c.shift && 'Shift', c.meta && 'Meta', key].filter(Boolean).join('+');
}

// ---- the registry ------------------------------------------------------------

/** Where an action can fire. Every shortcut today is `global` — the page's
 *  window handler is the only dispatcher — but two actions only conflict when
 *  their contexts can both be live, so a surface-local shortcut can share a chord
 *  with a global one that it never competes with. */
export type Context = 'global' | 'timeline' | 'preview';

export type GroupId = 'project' | 'edit' | 'tools' | 'playback' | 'markers' | 'view' | 'window' | 'help';

export const GROUPS: readonly { id: GroupId; label: string }[] = [
	{ id: 'project', label: 'Project' },
	{ id: 'edit', label: 'Editing' },
	{ id: 'tools', label: 'Tools' },
	{ id: 'playback', label: 'Playback' },
	{ id: 'markers', label: 'Markers and range' },
	{ id: 'view', label: 'Timeline view' },
	{ id: 'window', label: 'Workspaces and panels' },
	{ id: 'help', label: 'Help' }
];

export interface ActionDef {
	id: string;
	label: string;
	group: GroupId;
	context: Context;
	/** Portable chord spellings, in the order they are shown (the first is the one
	 *  a menu prints). */
	defaults: readonly string[];
	/** A line for the settings list when the label alone leaves a question. */
	hint?: string;
	/** `false`: one press is one action, and the auto-repeat of a held key is
	 *  swallowed — a held ⌘V would paste a dozen copies. Left out, the action
	 *  repeats (stepping a frame, zooming, undoing). */
	repeat?: boolean;
}

/** Bump when a default changes in a way a stored customisation has to be carried
 *  across (an action renamed, split or merged) and add the step to `MIGRATIONS`.
 *  A changed default chord alone needs no bump: actions the user never touched
 *  are not stored, so they follow it. */
export const KEYMAP_VERSION = 1;

const ACTION_LIST = [
	{ id: 'file.new', repeat: false, label: 'New project', group: 'project', context: 'global', defaults: ['Mod+N'] },
	{ id: 'file.open', repeat: false, label: 'Open project…', group: 'project', context: 'global', defaults: ['Mod+O'] },
	{
		id: 'file.save',
		repeat: false,
		label: 'Save project as…',
		group: 'project',
		context: 'global',
		defaults: ['Mod+S', 'Mod+Shift+S']
	},
	{ id: 'file.import', repeat: false, label: 'Import media…', group: 'project', context: 'global', defaults: ['Mod+I'] },
	{ id: 'file.export', repeat: false, label: 'Export…', group: 'project', context: 'global', defaults: ['Mod+E'] },
	{
		id: 'file.importCaptions',
		repeat: false,
		label: 'Import captions…',
		group: 'project',
		context: 'global',
		defaults: [],
		hint: 'Puts a .srt / .ass / .ssa file on the cut as captions, timed to the finished cut.'
	},
	{
		id: 'file.saveCover',
		repeat: false,
		label: 'Save cover frame…',
		group: 'project',
		context: 'global',
		defaults: [],
		hint: 'Writes the frame under the playhead, at the full delivery frame, as an image.'
	},
	{ id: 'app.settings', repeat: false, label: 'Settings', group: 'project', context: 'global', defaults: ['Mod+,'] },
	{ id: 'app.quit', repeat: false, label: 'Quit Kerf', group: 'project', context: 'global', defaults: [] },

	{ id: 'edit.undo', label: 'Undo', group: 'edit', context: 'global', defaults: ['Mod+Z'] },
	{ id: 'edit.redo', label: 'Redo', group: 'edit', context: 'global', defaults: ['Mod+Shift+Z', 'Mod+Y'] },
	{ id: 'edit.selectAll', label: 'Select all clips', group: 'edit', context: 'global', defaults: ['Mod+A'] },
	{ id: 'edit.copy', repeat: false, label: 'Copy', group: 'edit', context: 'global', defaults: ['Mod+C'] },
	{ id: 'edit.cut', repeat: false, label: 'Cut', group: 'edit', context: 'global', defaults: ['Mod+X'] },
	{ id: 'edit.paste', repeat: false, label: 'Paste at playhead', group: 'edit', context: 'global', defaults: ['Mod+V'] },
	{ id: 'edit.duplicate', repeat: false, label: 'Duplicate', group: 'edit', context: 'global', defaults: ['Mod+D'] },
	{
		id: 'edit.delete',
		repeat: false,
		label: 'Delete selection',
		group: 'edit',
		context: 'global',
		defaults: ['Delete', 'Backspace'],
		hint: 'Removes the selected title, or the selected clips and leaves their gaps.'
	},
	{
		id: 'edit.rippleDelete',
		repeat: false,
		label: 'Ripple delete selection',
		group: 'edit',
		context: 'global',
		defaults: ['Shift+Delete', 'Shift+Backspace'],
		hint: 'Removes the selected clips and closes the gaps behind them.'
	},
	{
		id: 'edit.trimStart',
		repeat: false,
		label: 'Trim start to playhead',
		group: 'edit',
		context: 'global',
		defaults: ['Q'],
		hint: 'Removes the part of the selected clip before the playhead. Follows ripple mode: on, the later clips close the gap.'
	},
	{
		id: 'edit.trimEnd',
		repeat: false,
		label: 'Trim end to playhead',
		group: 'edit',
		context: 'global',
		defaults: ['W'],
		hint: 'Removes the part of the selected clip after the playhead. Follows ripple mode: on, the later clips close the gap.'
	},
	{
		id: 'edit.detachAudio',
		repeat: false,
		label: 'Detach audio',
		group: 'edit',
		context: 'global',
		defaults: ['Shift+D'],
		hint: 'Gives each selected picture clip its own sound as a linked clip on an audio track, and mutes the picture — so it is heard once, from the audio track.'
	},
	{
		id: 'edit.reattachAudio',
		repeat: false,
		label: 'Reattach audio',
		group: 'edit',
		context: 'global',
		defaults: ['Mod+Shift+D'],
		hint: 'The way back: deletes the linked audio clip and lets the picture play its own sound again.'
	},
	{
		id: 'edit.link',
		repeat: false,
		label: 'Link clips',
		group: 'edit',
		context: 'global',
		defaults: ['Mod+L'],
		hint: 'Joins the selected clips (one per track) so a move, trim, split or delete of one is carried to the others. Alt on a drag, trim or razor leaves the links out for that edit.'
	},
	{
		id: 'edit.unlink',
		repeat: false,
		label: 'Unlink clips',
		group: 'edit',
		context: 'global',
		defaults: ['Mod+Shift+L'],
		hint: 'Takes the selected clips out of their link groups; each then edits on its own.'
	},
	{
		id: 'edit.clearSelection',
		label: 'Clear selection',
		group: 'edit',
		context: 'global',
		defaults: ['Escape'],
		hint: 'Escape also closes a menu or dialog and abandons a drag, whatever this is set to.'
	},

	{ id: 'tool.pointer', label: 'Select tool', group: 'tools', context: 'global', defaults: ['V'] },
	{ id: 'tool.razor', label: 'Razor tool', group: 'tools', context: 'global', defaults: ['C'] },
	{
		id: 'tool.roll',
		label: 'Roll tool',
		group: 'tools',
		context: 'global',
		defaults: ['N'],
		hint: 'Drag a cut between two touching clips to move it; nothing after the pair moves.'
	},
	{
		id: 'tool.slip',
		label: 'Slip tool',
		group: 'tools',
		context: 'global',
		defaults: ['Y'],
		hint: 'Drag a clip to show a different part of its footage in the same place.'
	},
	{
		id: 'tool.slide',
		label: 'Slide tool',
		group: 'tools',
		context: 'global',
		defaults: ['U'],
		hint: 'Drag a clip along its track; the clips touching it give way.'
	},
	{
		id: 'tool.snap',
		repeat: false,
		label: 'Toggle snapping',
		group: 'tools',
		context: 'global',
		defaults: ['S'],
		hint: 'While on, a drag snaps to the playhead, the ends of clips and titles, and the beats.'
	},
	{
		id: 'tool.rippleMode',
		repeat: false,
		label: 'Toggle ripple mode',
		group: 'tools',
		context: 'global',
		defaults: ['R'],
		hint: 'A project setting: while on, deleting and trimming close the gap behind.'
	},

	{ id: 'playback.toggle', repeat: false, label: 'Play / pause', group: 'playback', context: 'global', defaults: ['Space'] },
	{ id: 'playback.shuttleBack', repeat: false, label: 'Shuttle backward', group: 'playback', context: 'global', defaults: ['J'], hint: 'Tap again to double the speed, up to 8×.' },
	{ id: 'playback.pause', label: 'Pause', group: 'playback', context: 'global', defaults: ['K'] },
	{ id: 'playback.shuttleForward', repeat: false, label: 'Shuttle forward', group: 'playback', context: 'global', defaults: ['L'], hint: 'Tap again to double the speed, up to 8×.' },
	{ id: 'playback.stepBack', label: 'Back one frame', group: 'playback', context: 'global', defaults: ['ArrowLeft'] },
	{ id: 'playback.stepForward', label: 'Forward one frame', group: 'playback', context: 'global', defaults: ['ArrowRight'] },
	{ id: 'playback.jumpBack', label: 'Back one second', group: 'playback', context: 'global', defaults: ['Shift+ArrowLeft'] },
	{ id: 'playback.jumpForward', label: 'Forward one second', group: 'playback', context: 'global', defaults: ['Shift+ArrowRight'] },
	{ id: 'playback.toStart', label: 'Go to start', group: 'playback', context: 'global', defaults: ['Home'] },
	{ id: 'playback.toEnd', label: 'Go to end', group: 'playback', context: 'global', defaults: ['End'] },

	{ id: 'marker.add', repeat: false, label: 'Add marker at playhead', group: 'markers', context: 'global', defaults: ['M'] },
	{ id: 'marker.prev', label: 'Previous marker', group: 'markers', context: 'global', defaults: [','] },
	{ id: 'marker.next', label: 'Next marker', group: 'markers', context: 'global', defaults: ['.'] },
	{ id: 'range.markIn', label: 'Set in point', group: 'markers', context: 'global', defaults: ['I'] },
	{ id: 'range.markOut', label: 'Set out point', group: 'markers', context: 'global', defaults: ['O'] },
	{ id: 'range.clearIn', label: 'Clear in point', group: 'markers', context: 'global', defaults: ['Shift+I'] },
	{ id: 'range.clearOut', label: 'Clear out point', group: 'markers', context: 'global', defaults: ['Shift+O'] },

	{ id: 'view.zoomIn', label: 'Zoom in', group: 'view', context: 'global', defaults: ['+', '='] },
	{ id: 'view.zoomOut', label: 'Zoom out', group: 'view', context: 'global', defaults: ['-'] },
	{ id: 'view.zoomFit', label: 'Zoom to fit', group: 'view', context: 'global', defaults: ['Shift+Z'] },
	{ id: 'view.minimap', repeat: false, label: 'Show / hide the overview strip', group: 'view', context: 'global', defaults: [] },
	{
		id: 'view.safeAreas',
		repeat: false,
		label: 'Show / hide the safe-area guides',
		group: 'view',
		context: 'global',
		defaults: [],
		hint: 'Shades where a platform’s own interface covers a vertical or square cut. Only drawn while the project is cut for one.'
	},

	{ id: 'workspace.edit', repeat: false, label: 'Edit workspace', group: 'window', context: 'global', defaults: [] },
	{ id: 'workspace.color', repeat: false, label: 'Color workspace', group: 'window', context: 'global', defaults: [] },
	{ id: 'workspace.audio', repeat: false, label: 'Audio workspace', group: 'window', context: 'global', defaults: [] },
	{ id: 'workspace.motion', repeat: false, label: 'Motion workspace', group: 'window', context: 'global', defaults: [] },
	{ id: 'workspace.deliver', repeat: false, label: 'Deliver workspace', group: 'window', context: 'global', defaults: [] },
	{
		id: 'window.resetWorkspace',
		repeat: false,
		label: 'Reset this workspace',
		group: 'window',
		context: 'global',
		defaults: [],
		hint: 'Puts the panels of the workspace on screen back where its default has them, and its library tab.'
	},
	{ id: 'window.resetAllWorkspaces', repeat: false, label: 'Reset all workspaces', group: 'window', context: 'global', defaults: [] },
	{
		id: 'window.dockAll',
		repeat: false,
		label: 'Return all panels to the editor window',
		group: 'window',
		context: 'global',
		defaults: [],
		hint: 'Closes every window a panel was moved into and puts the panels back where they came from.'
	},

	{
		id: 'app.keyboard',
		repeat: false,
		label: 'Keyboard shortcuts',
		group: 'help',
		context: 'global',
		defaults: [],
		hint: 'Opens Settings on this list.'
	},
	{ id: 'app.checkUpdate', repeat: false, label: 'Check for updates…', group: 'help', context: 'global', defaults: [] },
	{ id: 'app.releases', repeat: false, label: 'Release page', group: 'help', context: 'global', defaults: [] },
	{ id: 'app.logs', repeat: false, label: 'Open the log folder', group: 'help', context: 'global', defaults: [] },
	{ id: 'app.about', repeat: false, label: 'About Kerf', group: 'help', context: 'global', defaults: [] }
] as const satisfies readonly ActionDef[];

/** Every action id — so a handler table typed `Record<ActionId, …>` fails to
 *  compile the day an action is added without one. */
export type ActionId = (typeof ACTION_LIST)[number]['id'];

export const ACTIONS: readonly ActionDef[] = ACTION_LIST;

export function actionDef(id: string, actions: readonly ActionDef[] = ACTIONS): ActionDef | undefined {
	return actions.find((a) => a.id === id);
}

/** Whether holding the key should keep firing the action (see `ActionDef.repeat`). */
export function allowsRepeat(id: string, actions: readonly ActionDef[] = ACTIONS): boolean {
	return actionDef(id, actions)?.repeat !== false;
}

/** Keys that mean the same thing everywhere and are not rebindable — shown
 *  read-only in the settings list so the layering is not a mystery. */
export const FIXED_KEYS: readonly { keys: string; does: string }[] = [
	{ keys: 'Esc', does: 'Abandons a drag, closes the open menu, panel or dialog' },
	{ keys: 'Tab', does: 'Moves focus (Shift+Tab moves back)' },
	{ keys: 'Enter / Space', does: 'Activates the focused button, tab or menu item' },
	{ keys: 'Alt, or F10', does: 'Focuses the menu bar; ← → move along it, ↓ opens a menu, Esc closes it' },
	{ keys: '← → ↑ ↓', does: 'Operate a focused slider, panel tab or list' },
	{ keys: 'Ctrl/⌘ + wheel', does: 'Zooms the timeline at the pointer' },
	{ keys: 'Shift / Ctrl/⌘ + click', does: 'Extends or toggles the clip selection' }
];

// ---- bindings ----------------------------------------------------------------

/** Each action's chords, in display order — what is actually in force. */
export type Bindings = Readonly<Record<string, readonly Chord[]>>;

/** Whether two contexts can both be live at once. */
export function contextsOverlap(a: Context, b: Context): boolean {
	return a === b || a === 'global' || b === 'global';
}

/** The chords an action has by default, parsed. */
function defaultChords(a: ActionDef, platform: Platform): Chord[] {
	return a.defaults.flatMap((d) => parseChord(d, platform) ?? []);
}

/** What is stored: only the actions the user changed, as portable spellings. An
 *  empty list is a real choice — the action is deliberately unbound. */
export interface KeyOverrides {
	/** The registry version these were written against. */
	version: number;
	bindings: Record<string, string[]>;
}

export const emptyOverrides = (version = KEYMAP_VERSION): KeyOverrides => ({ version, bindings: {} });

export const hasOverrides = (o: KeyOverrides): boolean => Object.keys(o.bindings).length > 0;

/** The chords every action has with `overrides` applied.
 *
 *  A customised action keeps exactly what the user set. An untouched one has its
 *  defaults, minus any default the user has since given to another action — a
 *  customisation always beats a default, so a build that adds a default someone
 *  already uses for something else does not steal their key. Whatever still
 *  collides after that (a hand-edited file) goes to the earlier action in
 *  registry order, so what fires is never ambiguous. */
export function resolveBindings(overrides: KeyOverrides, platform: Platform, actions: readonly ActionDef[] = ACTIONS): Bindings {
	const custom = new Set<string>();
	const resolved: Record<string, Chord[]> = {};
	for (const a of actions) {
		const stored = overrides.bindings[a.id];
		if (stored) {
			custom.add(a.id);
			resolved[a.id] = dedupe(stored.flatMap((s) => parseChord(s, platform) ?? []));
		} else {
			resolved[a.id] = defaultChords(a, platform);
		}
	}
	// A default yields to a customisation of another action…
	for (const a of actions) {
		if (custom.has(a.id)) continue;
		resolved[a.id] = resolved[a.id].filter(
			(c) => !actions.some((b) => b.id !== a.id && custom.has(b.id) && contextsOverlap(a.context, b.context) && resolved[b.id].some((x) => sameChord(x, c)))
		);
	}
	// …and what is left is first come, first served.
	const taken: { chord: Chord; context: Context }[] = [];
	for (const a of actions) {
		resolved[a.id] = resolved[a.id].filter((c) => {
			if (taken.some((t) => sameChord(t.chord, c) && contextsOverlap(t.context, a.context))) return false;
			taken.push({ chord: c, context: a.context });
			return true;
		});
	}
	return resolved;
}

function dedupe(chords: Chord[]): Chord[] {
	return chords.filter((c, i) => chords.findIndex((d) => sameChord(c, d)) === i);
}

/** The action a keypress is, or null. `contexts` are the ones live where it
 *  happened (default: all of them, which is every shortcut today). */
export function matchAction(
	e: KeyEventLike,
	bindings: Bindings,
	actions: readonly ActionDef[] = ACTIONS,
	contexts: readonly Context[] = ['global', 'timeline', 'preview']
): string | null {
	const chord = chordFromEvent(e);
	if (!chord) return null;
	for (const a of actions) {
		if (!contexts.includes(a.context)) continue;
		if (bindings[a.id]?.some((c) => sameChord(c, chord))) return a.id;
	}
	return null;
}

/** The *other* actions that already use `chord` where `id` would — what the
 *  settings list names when a new chord collides. */
export function conflictsFor(bindings: Bindings, id: string, chord: Chord, actions: readonly ActionDef[] = ACTIONS): ActionDef[] {
	const own = actionDef(id, actions);
	if (!own) return [];
	return actions.filter((a) => a.id !== id && contextsOverlap(a.context, own.context) && bindings[a.id]?.some((c) => sameChord(c, chord)));
}

/** Every chord two or more actions share, for a registry that has to be clean. */
export function findCollisions(bindings: Bindings, actions: readonly ActionDef[] = ACTIONS): { chord: Chord; ids: string[] }[] {
	const out: { chord: Chord; ids: string[] }[] = [];
	for (const a of actions) {
		for (const c of bindings[a.id] ?? []) {
			const ids = actions.filter((b) => contextsOverlap(a.context, b.context) && bindings[b.id]?.some((x) => sameChord(x, c))).map((b) => b.id);
			if (ids.length > 1 && !out.some((o) => sameChord(o.chord, c) && o.ids.join() === ids.join())) out.push({ chord: c, ids });
		}
	}
	return out;
}

// ---- stored overrides: reading, migrating, editing ---------------------------

/** One step from the version before it: given the stored lists of the old shape,
 *  the lists of the new. Renames (`{ 'edit.delete': … }` → `'clip.delete'`) and
 *  splits go here; a step never has to drop unknown ids, validate chords or
 *  prune what equals a default — `parseKeyOverrides` does that afterwards. */
export interface Migration {
	/** The version this step produces. */
	to: number;
	apply(bindings: Record<string, string[]>): Record<string, string[]>;
}

/** Steps from the version a file was written at to `KEYMAP_VERSION`, in order.
 *  Version 1 is where it starts, so there is nothing to migrate from yet. */
export const MIGRATIONS: readonly Migration[] = [];

export interface ParseOptions {
	actions?: readonly ActionDef[];
	version?: number;
	migrations?: readonly Migration[];
}

/** Read stored overrides — whatever the file holds — into ones that can be
 *  trusted: carried forward from the version they were written at, with unknown
 *  actions and unreadable chords dropped, and anything equal to the defaults
 *  forgotten (so that action follows the defaults from here on).
 *
 *  Never throws: a preference is not worth a failed launch. An action whose
 *  stored chords are *all* unreadable reverts to its defaults rather than
 *  becoming unbound, since a mangled entry is not a decision to unbind; a stored
 *  empty list is that decision, and is kept. */
export function parseKeyOverrides(raw: unknown, platform: Platform, opts: ParseOptions = {}): KeyOverrides {
	const actions = opts.actions ?? ACTIONS;
	const current = opts.version ?? KEYMAP_VERSION;
	const migrations = opts.migrations ?? MIGRATIONS;
	if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return emptyOverrides(current);
	const obj = raw as Record<string, unknown>;
	const written = typeof obj.version === 'number' && Number.isInteger(obj.version) && obj.version >= 1 ? obj.version : 1;
	let bindings: Record<string, string[]> = {};
	if (typeof obj.bindings === 'object' && obj.bindings !== null && !Array.isArray(obj.bindings)) {
		for (const [id, value] of Object.entries(obj.bindings)) {
			if (Array.isArray(value)) bindings[id] = value.filter((v): v is string => typeof v === 'string');
		}
	}
	// A file from a newer build keeps what this one understands; there is no
	// way to migrate *down*.
	for (const m of [...migrations].sort((a, b) => a.to - b.to)) {
		if (m.to > written && m.to <= current) bindings = m.apply(bindings);
	}

	const out: Record<string, string[]> = {};
	for (const a of actions) {
		const stored = bindings[a.id];
		if (!stored) continue;
		const chords = dedupe(stored.flatMap((s) => parseChord(s, platform) ?? []));
		if (stored.length > 0 && chords.length === 0) continue;
		const next = chords.map((c) => formatChord(c, platform));
		if (sameList(next, canonicalDefaults(a, platform))) continue;
		out[a.id] = next;
	}
	return { version: current, bindings: out };
}

function canonicalDefaults(a: ActionDef, platform: Platform): string[] {
	return defaultChords(a, platform).map((c) => formatChord(c, platform));
}

const sameList = (a: readonly string[], b: readonly string[]) => a.length === b.length && a.every((x, i) => x === b[i]);

/** What to write: `null` when nothing is customised, so the settings file stays clean. */
export function serializeOverrides(o: KeyOverrides): KeyOverrides | null {
	return hasOverrides(o) ? o : null;
}

/** Give an action exactly these chords. Equal to its defaults is not an
 *  override — the entry is dropped so the action follows them from here on. */
export function setActionChords(
	o: KeyOverrides,
	id: string,
	chords: readonly Chord[],
	platform: Platform,
	actions: readonly ActionDef[] = ACTIONS
): KeyOverrides {
	const a = actionDef(id, actions);
	if (!a) return o;
	const next = dedupe([...chords]).map((c) => formatChord(c, platform));
	const bindings = { ...o.bindings };
	if (sameList(next, canonicalDefaults(a, platform))) delete bindings[id];
	else bindings[id] = next;
	return { ...o, bindings };
}

/** Put one action back to its defaults. */
export function resetAction(o: KeyOverrides, id: string): KeyOverrides {
	if (!(id in o.bindings)) return o;
	const bindings = { ...o.bindings };
	delete bindings[id];
	return { ...o, bindings };
}

export const resetAll = (o: KeyOverrides): KeyOverrides => emptyOverrides(o.version);

/** A new chord for an action: in place of `replacing`, or alongside its others
 *  when that is null. */
export interface Rebind {
	id: string;
	chord: Chord;
	replacing: Chord | null;
}

/** How a collision is settled: the other action takes the chord this one gave up
 *  (`swap`, which needs something to give), or just loses it (`unbind`). */
export type Resolution = 'swap' | 'unbind';

/** The actions `rebind` would take the chord from. */
export function rebindConflicts(bindings: Bindings, rebind: Rebind, actions: readonly ActionDef[] = ACTIONS): ActionDef[] {
	return conflictsFor(bindings, rebind.id, rebind.chord, actions);
}

/** Apply a rebind. With a collision, `resolution` says what happens to the
 *  action that held the chord, and without one nothing changes: the caller has
 *  to choose, never this. */
export function applyRebind(
	o: KeyOverrides,
	bindings: Bindings,
	rebind: Rebind,
	resolution: Resolution | null,
	platform: Platform,
	actions: readonly ActionDef[] = ACTIONS
): KeyOverrides {
	const { id, chord, replacing } = rebind;
	const conflicts = rebindConflicts(bindings, rebind, actions);
	if (conflicts.length > 0 && !resolution) return o;
	const own = bindings[id] ?? [];
	const mine = replacing
		? own.map((c) => (sameChord(c, replacing) ? chord : c))
		: [...own, chord];
	let next = setActionChords(o, id, mine, platform, actions);
	for (const other of conflicts) {
		const theirs = bindings[other.id] ?? [];
		const moved =
			resolution === 'swap' && replacing
				? theirs.map((c) => (sameChord(c, chord) ? replacing : c))
				: theirs.filter((c) => !sameChord(c, chord));
		next = setActionChords(next, other.id, moved, platform, actions);
	}
	return next;
}

// ---- putting an action back ---------------------------------------------------

/** Whether an action has anything other than its defaults, in force — which is
 *  not the same as having an override: another action's customisation can have
 *  taken one of its default chords, and Reset has to stay on offer for that. */
export function differsFromDefault(bindings: Bindings, id: string, platform: Platform, actions: readonly ActionDef[] = ACTIONS): boolean {
	const a = actionDef(id, actions);
	if (!a) return false;
	const want = defaultChords(a, platform);
	const have = bindings[id] ?? [];
	return want.length !== have.length || want.some((c, i) => !sameChord(c, have[i]));
}

/** What putting an action back would run into. */
export interface ResetPlan {
	/** The other actions holding one of its default chords. */
	conflicts: ActionDef[];
	/** The first default chord that is held elsewhere, for the prompt to name. */
	chord: Chord | null;
	/** A chord it has now that is not a default, which a swap hands over. */
	gives: Chord | null;
}

export function resetPlan(bindings: Bindings, id: string, platform: Platform, actions: readonly ActionDef[] = ACTIONS): ResetPlan {
	const own = actionDef(id, actions);
	if (!own) return { conflicts: [], chord: null, gives: null };
	const want = defaultChords(own, platform);
	const isDefault = (c: Chord) => want.some((w) => sameChord(w, c));
	const conflicts = actions.filter(
		(b) => b.id !== id && contextsOverlap(b.context, own.context) && bindings[b.id]?.some(isDefault)
	);
	const chord = want.find((w) => conflicts.some((b) => bindings[b.id]?.some((c) => sameChord(c, w)))) ?? null;
	return { conflicts, chord, gives: (bindings[id] ?? []).find((c) => !isDefault(c)) ?? null };
}

/** Put an action back to its defaults. If another action has since been given
 *  one of them, `resolution` says what happens to it — the same choice as a
 *  rebind, because it is one — and without a resolution nothing changes. */
export function applyReset(
	o: KeyOverrides,
	bindings: Bindings,
	id: string,
	resolution: Resolution | null,
	platform: Platform,
	actions: readonly ActionDef[] = ACTIONS
): KeyOverrides {
	const own = actionDef(id, actions);
	if (!own) return o;
	const plan = resetPlan(bindings, id, platform, actions);
	if (plan.conflicts.length > 0 && !resolution) return o;
	const want = defaultChords(own, platform);
	const isDefault = (c: Chord) => want.some((w) => sameChord(w, c));
	// What it gives up, in order, for a swap to hand out.
	const pool = (bindings[id] ?? []).filter((c) => !isDefault(c));
	let next = resetAction(o, id);
	for (const other of plan.conflicts) {
		const moved: Chord[] = [];
		for (const c of bindings[other.id] ?? []) {
			if (!isDefault(c)) moved.push(c);
			else if (resolution === 'swap') {
				const give = pool.shift();
				if (give) moved.push(give);
			}
		}
		next = setActionChords(next, other.id, moved, platform, actions);
	}
	return next;
}

// ---- the settings list -------------------------------------------------------

/** The actions that match a search, in registry order. Every word has to appear
 *  in the label, group, id or any chord (as shown or as stored), so `ctrl z`,
 *  `ripple` and `shuttle` all find what they say. An empty query is everything. */
export function filterActions(query: string, bindings: Bindings, platform: Platform, actions: readonly ActionDef[] = ACTIONS): ActionDef[] {
	const words = query.toLowerCase().split(/\s+/).filter(Boolean);
	if (words.length === 0) return [...actions];
	return actions.filter((a) => {
		const chords = bindings[a.id] ?? [];
		const hay = [
			a.label,
			a.hint ?? '',
			GROUPS.find((g) => g.id === a.group)?.label ?? '',
			a.id,
			...chords.map((c) => displayChord(c, platform)),
			...chords.map((c) => formatChord(c, platform))
		]
			.join(' ')
			.toLowerCase();
		return words.every((w) => hay.includes(w));
	});
}
