// Long lists are read a page at a time: the edit history and the agent's task queue both grow for as long as a
// project lives and are re-read after every edit. The backend windows them from the newest end (`limit` /
// `offset`, `Page`), so a list is held as "the newest N" and a button asks for more. Pure — the undo / redo rules
// that have to cope with a partial history are here so a test holds them.

import type { Revision } from './types';

/** Edits read at a time (and added by "Show older edits"). */
export const HISTORY_PAGE = 100;

/** Tasks read at a time (and added by "Show older tasks"). */
export const TASK_PAGE = 30;

/** The newest `n` of an oldest-first list (all of it when it is shorter). */
export function keepNewest<T>(list: readonly T[], n: number): T[] {
	return list.length > n ? list.slice(list.length - n) : [...list];
}

/** Whether there is an edit before the current one. With the head inside the page it is the neighbour; at the page's
 *  oldest edit it is whether older ones are behind it; with the head older than the whole page (reverted far back) there
 *  certainly are none newer, and older ones are asked of the backend. */
export function undoable(history: readonly Pick<Revision, 'current'>[], more: boolean): boolean {
	const i = history.findIndex((r) => r.current);
	if (i < 0) return history.length > 0;
	return i > 0 || (i === 0 && more);
}

/** Whether there is an edit after the current one. A head older than the page has the whole page after it. */
export function redoable(history: readonly Pick<Revision, 'current'>[]): boolean {
	const i = history.findIndex((r) => r.current);
	if (i < 0) return history.length > 0;
	return i < history.length - 1;
}
