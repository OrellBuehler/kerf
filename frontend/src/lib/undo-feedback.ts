// What Undo and Redo are called and what they say — pure. The menu names the edit it would take back
// ("Undo Ripple delete"), and a press says in a line what it did, so a key that rewinds the project is not
// silent. A held key (or a quick run of presses) is one burst, one line that counts up: not a toast per press.

import type { Revision } from './types';

/** The edit Undo would take back — the one the project is at now — or null when there is none to take back.
 *  `more` is whether older edits exist beyond the page held (`editor.historyMore`). */
export function undoLabel(history: readonly Pick<Revision, 'current' | 'label'>[], more = false): string | null {
	const i = history.findIndex((r) => r.current);
	if (i < 0) return null;
	return i > 0 || more ? history[i].label || null : null;
}

/** The edit Redo would bring back — the one after the project's head — or null. */
export function redoLabel(history: readonly Pick<Revision, 'current' | 'label'>[]): string | null {
	const i = history.findIndex((r) => r.current);
	return i >= 0 ? history[i + 1]?.label || null : null;
}

/** A menu entry's text with the edit it acts on: `Undo Ripple delete`. The label is the backend's, with its
 *  capital — an edit named in the middle of a menu entry reads better as it was said, so it is left alone. */
export function withLabel(verb: 'Undo' | 'Redo', label: string | null): string {
	return label ? `${verb} ${label}` : verb;
}

/** Presses closer together than this are one burst. */
export const BURST_MS = 1400;

/** The revision the project is at — the one `current` marks — or null when the page held does not show it. */
export function headSeq(history: readonly Pick<Revision, 'current' | 'seq'>[]): number | null {
	return history.find((r) => r.current)?.seq ?? null;
}

/** Whether a rejection is the history saying it has nowhere further to go (`nothing to undo` / `nothing to redo`, the
 *  backend's own words): the end of the line, which is said quietly, not a failure. */
export const isEndOfHistory = (e: unknown): boolean => /nothing to (undo|redo)/i.test(e instanceof Error ? e.message : String(e));

export interface Burst {
	kind: 'undo' | 'redo';
	/** The revision the project was at when the last press had done: a press continues the burst only if the project is
	 *  still there, i.e. nothing else was edited in between. */
	head: number | null;
	/** How many presses so far. */
	count: number;
	/** The label of the first press's edit: what the project was before this burst began... */
	first: string | null;
	/** ...and of the latest: where it has got to. */
	last: string | null;
	at: number;
}

/** The burst a press at `now` belongs to: the running one, counted up, or a new one — when it is the other direction,
 *  came after a pause, or the project is not where the burst left it (`before`, the revision it stood at as the press
 *  began, is not the burst's `head`: an edit, or a jump in the history, came between — "Undid 2 edits" after undo, edit,
 *  undo would be false). `after` is the revision it stands at now. */
export function nextBurst(
	prev: Burst | null,
	kind: 'undo' | 'redo',
	label: string | null,
	now: number,
	before: number | null = null,
	after: number | null = null
): Burst {
	const same = prev && prev.kind === kind && now - prev.at <= BURST_MS && prev.head === before;
	if (same) return { ...prev, head: after, count: prev.count + 1, last: label ?? prev.last, at: now };
	return { kind, head: after, count: 1, first: label, last: label, at: now };
}

/** The line for a burst: `Undid: Split clip`, `Undid 3 edits, back to before Ripple delete`. */
export function burstLine(b: Burst): string {
	const verb = b.kind === 'undo' ? 'Undid' : 'Redid';
	if (b.count === 1) return b.first ? `${verb}: ${b.first}` : verb;
	const edits = `${b.count} edits`;
	// An undo ends before the oldest edit it took back; a redo ends after the newest it brought back.
	if (b.kind === 'undo') return b.last ? `${verb} ${edits}, back to before ${b.last}` : `${verb} ${edits}`;
	return b.last ? `${verb} ${edits}, up to ${b.last}` : `${verb} ${edits}`;
}

/** What nothing to undo / redo says. */
export const NOTHING_TO_UNDO = 'Nothing to undo';
export const NOTHING_TO_REDO = 'Nothing to redo';
