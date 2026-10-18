// The read-only shortcut cheat sheet (`?`) — pure. It lists what is *in force* — the registry's defaults with the
// user's changes on top, resolved by `resolveBindings` — grouped as the settings list groups them, and leaves out
// the actions that have no key (a sheet is for finding the key; the actions without one are in the menus and,
// to give them one, in the Keyboard settings, which is where rebinding lives and this does not).

import { ACTIONS, FIXED_KEYS, GROUPS, displayChord, plainLabel, type ActionDef, type Bindings, type Platform } from './keymap';

export interface SheetRow {
	id: string;
	label: string;
	/** Each key as it prints (`Ctrl+K`, `⌘K`), in the order the settings list shows them. */
	keys: string[];
}

export interface SheetGroup {
	id: string;
	label: string;
	rows: SheetRow[];
}

export interface Sheet {
	groups: SheetGroup[];
	/** The keys that mean the same everywhere and cannot be changed. */
	fixed: readonly { keys: string; does: string }[];
	/** How many actions have no key, for the line that points at the settings. */
	unbound: number;
}

export function shortcutSheet(bindings: Bindings, platform: Platform, actions: readonly ActionDef[] = ACTIONS): Sheet {
	let unbound = 0;
	const groups = GROUPS.map((g) => {
		const rows: SheetRow[] = [];
		for (const a of actions) {
			if (a.group !== g.id) continue;
			const keys = (bindings[a.id] ?? []).map((c) => displayChord(c, platform));
			if (keys.length === 0) unbound++;
			else rows.push({ id: a.id, label: plainLabel(a.label), keys });
		}
		return { id: g.id, label: g.label, rows };
	}).filter((g) => g.rows.length > 0);
	return { groups, fixed: FIXED_KEYS, unbound };
}

/** `3 more actions have no shortcut`, or nothing when every one has. */
export function unboundLine(n: number): string | null {
	return n === 0 ? null : `${n} more action${n === 1 ? ' has' : 's have'} no shortcut — they are in the menus, and Customize keyboard shortcuts… gives them one.`;
}
