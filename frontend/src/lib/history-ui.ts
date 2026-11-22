// Edit › Show edit history: bring up the list of every edit, where it lives today. The history is the Agent
// panel's last section (`AgentPanel.svelte`); this brings that panel forward and scrolls its History heading
// into view. It finds the section by a hook the panel can put wherever the history ends up — an element with
// `data-history` — and, until it does, by the heading's own text, so the panel itself is not touched. The glue that
// brings the panel forward is `showHistory` in `menu-commands.ts` (it needs the workspace singleton).

/** What marks the history section, wherever the panel that holds it puts it. */
export const HISTORY_HOOK = '[data-history]';

/** The part of a document this looks through (so a test needs no DOM). */
export interface Searchable {
	querySelector(selector: string): Element | null;
	querySelectorAll(selector: string): Iterable<Element>;
}

/** The history section: the marked element, else the heading that says "History". */
export function findHistory(root: Searchable): Element | null {
	const marked = root.querySelector(HISTORY_HOOK);
	if (marked) return marked;
	for (const el of root.querySelectorAll('.k-overline')) if (el.textContent?.trim().toLowerCase() === 'history') return el;
	return null;
}
