// How the keyboard moves about a menu, as pure functions over a list of rows so the menu bar and the
// context menu share one rule and it can be tested without a DOM (the ARIA menu pattern: arrows
// wrap, Home / End jump, a letter goes to the next row that starts with it, Enter / Space run,
// Escape closes and gives focus back).

/** What navigation needs to know about a row. A separator, a header or a fact row is not one that
 *  can take focus; a *disabled* row is — it says why when asked, which is the menu pattern's rule —
 *  but running it does nothing. */
export interface NavRow {
	focusable: boolean;
	label: string;
	/** Can be run (default: yes). A disabled row still takes focus, but is not where a menu starts. */
	enabled?: boolean;
}

/** The rows that can take focus. */
export function focusableRows(rows: readonly NavRow[]): number[] {
	return rows.flatMap((r, i) => (r.focusable ? [i] : []));
}

/** The row `by` steps from `at`, wrapping at both ends; `at` when there is nothing to move to. From
 *  nowhere (`-1`, or a row that cannot take focus) down goes to the first and up to the last. */
export function stepRow(rows: readonly NavRow[], at: number, by: 1 | -1): number {
	const idx = focusableRows(rows);
	if (idx.length === 0) return at;
	const pos = idx.indexOf(at);
	if (pos < 0) return by === 1 ? idx[0] : idx[idx.length - 1];
	return idx[(pos + by + idx.length) % idx.length];
}

/** The first row that can take focus, or -1. */
export function firstRow(rows: readonly NavRow[]): number {
	return focusableRows(rows)[0] ?? -1;
}

/** Where a menu opened from the keyboard starts: the first row that can be *run*, else the first that
 *  can take focus, or -1. */
export function startRow(rows: readonly NavRow[]): number {
	const idx = focusableRows(rows);
	return idx.find((i) => rows[i].enabled !== false) ?? idx[0] ?? -1;
}

/** The last row that can take focus, or -1. */
export function lastRow(rows: readonly NavRow[]): number {
	return focusableRows(rows).at(-1) ?? -1;
}

/** The next row after `at` whose label starts with `ch` (case-insensitive), wrapping round to
 *  `at` itself last; null when none does. */
export function typeaheadRow(rows: readonly NavRow[], at: number, ch: string): number | null {
	const idx = focusableRows(rows);
	const order = [...idx.filter((i) => i > at), ...idx.filter((i) => i <= at)];
	const want = ch.toLowerCase();
	return order.find((i) => rows[i].label.trim().toLowerCase().startsWith(want)) ?? null;
}

export type MenuKey =
	| { do: 'next' }
	| { do: 'prev' }
	| { do: 'first' }
	| { do: 'last' }
	| { do: 'run' }
	| { do: 'close' }
	/** Tab leaves the menu (it does not cycle inside it) and goes on. */
	| { do: 'leave' }
	| { do: 'type'; ch: string };

export interface KeyLike {
	key: string;
	ctrlKey?: boolean;
	metaKey?: boolean;
	altKey?: boolean;
	shiftKey?: boolean;
}

/** What a key does in a vertical menu, or null when the menu does not take it (a chord goes on to
 *  whoever wanted it). */
export function menuKey(e: KeyLike): MenuKey | null {
	if (e.ctrlKey || e.metaKey || e.altKey) return null;
	switch (e.key) {
		case 'ArrowDown':
			return { do: 'next' };
		case 'ArrowUp':
			return { do: 'prev' };
		case 'Home':
			return { do: 'first' };
		case 'End':
			return { do: 'last' };
		case 'Enter':
		case ' ':
			return { do: 'run' };
		case 'Escape':
			return { do: 'close' };
		case 'Tab':
			return { do: 'leave' };
	}
	// One printable character, a letter or a digit; Shift only makes it a capital.
	return [...e.key].length === 1 && /[\p{L}\p{N}]/u.test(e.key) ? { do: 'type', ch: e.key } : null;
}

/** Where focus goes for a key: the row to focus, or null when the key does not move it. `at` is the
 *  row that has focus now (-1 for none). */
export function rowFor(rows: readonly NavRow[], at: number, k: MenuKey): number | null {
	switch (k.do) {
		case 'next':
			return stepRow(rows, at, 1);
		case 'prev':
			return stepRow(rows, at, -1);
		case 'first': {
			const i = firstRow(rows);
			return i < 0 ? null : i;
		}
		case 'last': {
			const i = lastRow(rows);
			return i < 0 ? null : i;
		}
		case 'type':
			return typeaheadRow(rows, at, k.ch);
		default:
			return null;
	}
}

/** What a key does in a group of radio buttons: the arrows move to the neighbour (both axes — it is
 *  a row of chips, not a column), Home / End jump, selection follows focus. Anything else is the
 *  page's. */
export function radioKey(e: KeyLike): MenuKey | null {
	if (e.ctrlKey || e.metaKey || e.altKey || e.shiftKey) return null;
	switch (e.key) {
		case 'ArrowDown':
		case 'ArrowRight':
			return { do: 'next' };
		case 'ArrowUp':
		case 'ArrowLeft':
			return { do: 'prev' };
		case 'Home':
			return { do: 'first' };
		case 'End':
			return { do: 'last' };
	}
	return null;
}

/** Which radio button holds the group's one tab stop: the chosen one, when it can take focus; else the
 *  first that can. (Preview source disables every option but "Always original" while proxies are off,
 *  and the chosen one may be a disabled other — a tab stop on a disabled chip is no tab stop at all.)
 *  -1 when none can. */
export function tabStop(radios: readonly { checked: boolean; enabled: boolean }[]): number {
	const chosen = radios.findIndex((r) => r.checked && r.enabled);
	return chosen >= 0 ? chosen : radios.findIndex((r) => r.enabled);
}
