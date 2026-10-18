// What a settings row asks to know: is the search box letting me through? The dialog provides it, every
// section component and `SettingRow` reads it (so a section never has to be handed the query).

import { getContext, setContext } from 'svelte';

export interface SettingsFilter {
	/** The trimmed query ('' when the box is empty). */
	readonly query: string;
	/** Whether the setting `id` shows: always without a query, else only when it matches. */
	shows(id: string): boolean;
}

const KEY = Symbol('settings-filter');

export function provideFilter(filter: SettingsFilter): void {
	setContext(KEY, filter);
}

const OPEN: SettingsFilter = { query: '', shows: () => true };

export function useFilter(): SettingsFilter {
	return getContext<SettingsFilter | undefined>(KEY) ?? OPEN;
}
