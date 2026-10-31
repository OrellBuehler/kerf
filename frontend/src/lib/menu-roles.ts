/* What a context-menu row is to assistive technology — pure. An action row is a `menuitem`; a row that is a choice
 * (a height, a delivery shape, a tag in the filter) is a `menuitemcheckbox` or `menuitemradio` with `aria-checked`,
 * as the title-bar menus' are. The tick the eye sees is the same fact, drawn where the icon goes. */

import type { MenuItem } from './context-menu.svelte';

type Row = Extract<MenuItem, { type?: 'item' }>;

export type RowRole = 'menuitem' | 'menuitemcheckbox' | 'menuitemradio';

/** The ARIA role of an action row: a `radio` is a radio, a `check` — or any row that has a `checked` and says no more — a checkbox. */
export function rowRole(item: Pick<Row, 'role' | 'checked'>): RowRole {
	if (item.role === 'radio') return 'menuitemradio';
	if (item.role === 'check' || item.checked !== undefined) return 'menuitemcheckbox';
	return 'menuitem';
}

/** `aria-checked` of a row: none for an action, else whether it is the one that is on. */
export function rowChecked(item: Pick<Row, 'role' | 'checked'>): boolean | undefined {
	return rowRole(item) === 'menuitem' ? undefined : !!item.checked;
}

/** The icon a row draws: a tick while it is the one that is on, else its own icon. */
export function rowIcon(item: Pick<Row, 'role' | 'checked' | 'icon'>): string | undefined {
	return item.checked ? 'check' : item.icon;
}
