/* The edit history as the Agent panel's History section draws it — pure. The revisions come newest first from
 * `editor.history`; what the section adds is who made each one, when, and where it stands against the head: the revision
 * the cut is at is *current*, the ones before it are *past*, and the ones after it are *undone* — still in the project,
 * redoable, until a new edit drops them (`record_revision` deletes everything after the head). Going back itself loses
 * nothing and asks nothing; the edit that follows does — and applying an agent's proposal is an edit, so before one
 * lands over undone revisions the user is told which they are (`undoneBranch`). */

import type { EditSource, Revision } from './types';

export type RevisionState = 'current' | 'past' | 'undone';

export interface HistoryRow {
	seq: number;
	label: string;
	source: EditSource;
	/** Who made it, as the row says: `You`, `Agent`, `Kerf` (the baseline and what the app itself did). */
	who: string;
	/** When it was made, as the backend stamped it: the row says how long ago from this (`relativeTime`) as time passes,
	 *  without the rows being made again. */
	iso: string;
	/** The exact time, for the tooltip over the "how long ago". */
	at: string;
	state: RevisionState;
}

const WHO: Record<EditSource, string> = { user: 'You', agent: 'Agent', system: 'Kerf' };

/** `just now`, `4 min ago`, `2 h ago`, then the date; empty for a time that cannot be read. */
export function relativeTime(iso: string, now: number): string {
	const t = Date.parse(iso);
	if (!Number.isFinite(t)) return '';
	const secs = Math.max(0, Math.round((now - t) / 1000));
	if (secs < 45) return 'just now';
	const mins = Math.round(secs / 60);
	if (mins < 60) return `${Math.max(1, mins)} min ago`;
	const hours = Math.round(mins / 60);
	if (hours < 24) return `${hours} h ago`;
	return new Date(t).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

/** The exact time of a revision for its tooltip, in the user's own format; the raw stamp if it cannot be read. */
export function absoluteTime(iso: string): string {
	const t = Date.parse(iso);
	return Number.isFinite(t) ? new Date(t).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'medium' }) : iso;
}

/** The revisions as rows, newest first. A head the page does not hold (reverted far back, past the older ones asked for)
 *  leaves every row after it. */
export function historyRows(revisions: readonly Revision[]): HistoryRow[] {
	const head = revisions.find((r) => r.current)?.seq;
	return [...revisions]
		.sort((a, b) => b.seq - a.seq)
		.map((r) => ({
			seq: r.seq,
			label: r.label,
			source: r.source,
			who: WHO[r.source] ?? 'Kerf',
			iso: r.created_at,
			at: absoluteTime(r.created_at),
			state: r.current ? 'current' : head !== undefined && r.seq > head ? 'undone' : head === undefined ? 'undone' : 'past'
		}));
}

/** The revisions after the one the cut is at: what redo would walk through, and what the next edit — an applied proposal
 *  included — would drop for good. `exact` is false when the head is not in the page read (stepped back further than the page
 *  reaches): every revision read is then after it, and there may be more. */
export interface UndoneBranch {
	count: number;
	/** Oldest first, as they were made. */
	labels: string[];
	exact: boolean;
}

export function undoneBranch(revisions: readonly Pick<Revision, 'current' | 'seq' | 'label'>[]): UndoneBranch {
	if (revisions.length === 0) return { count: 0, labels: [], exact: true };
	const head = revisions.find((r) => r.current)?.seq;
	const after = (head === undefined ? [...revisions] : revisions.filter((r) => r.seq > head)).sort((a, b) => a.seq - b.seq);
	return { count: after.length, labels: after.map((r) => r.label), exact: head !== undefined };
}

/** How many are undone (see `undoneBranch`). */
export const undoneCount = (revisions: readonly Pick<Revision, 'current' | 'seq' | 'label'>[]): number => undoneBranch(revisions).count;

const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? '' : 's'}`;

/** `Remove clip`, `Remove clip and Trim clip`, `Remove clip, Trim clip and 2 more`: which edits, in a sentence. */
export function branchSentence(labels: readonly string[], show = 3): string {
	const named = labels.map((l) => `“${l}”`);
	if (named.length <= 1) return named[0] ?? '';
	if (named.length === 2) return `${named[0]} and ${named[1]}`;
	if (named.length <= show) return `${named.slice(0, -1).join(', ')} and ${named[named.length - 1]}`;
	return `${named.slice(0, show).join(', ')} and ${named.length - show} more`;
}

/** The note over the list while the cut is not at the newest revision: how far back, how long the edits after it are kept
 *  (until the next edit — an applied proposal is one), and that the next edit drops them. */
export function backNote(branch: UndoneBranch | number): string | null {
	const b = typeof branch === 'number' ? { count: branch, labels: [], exact: true } : branch;
	if (b.count <= 0) return null;
	const n = b.exact ? plural(b.count, 'edit') : `at least ${plural(b.count, 'edit')}`;
	return `You are ${n} back. What comes after this point is kept only until you make a new edit; applying an agent’s proposal is one, and it drops ${b.count === 1 && b.exact ? 'it' : 'them'}.`;
}

/** What a row's button says: it is a move of the head, back (`past`) or forward (`undone`). */
export function goLabel(state: RevisionState): string {
	return state === 'undone' ? 'Redo to here' : 'Go back';
}

export function goTip(state: RevisionState, later: number): string {
	if (state === 'undone') return 'Move the cut forward to this edit';
	return later > 0
		? `Put the cut back as it was at this edit. The ${plural(later, 'edit')} after it ${later === 1 ? 'is' : 'are'} kept only until you make a new edit.`
		: 'Put the cut back as it was at this edit';
}

/** The button's accessible name: what it does, to which revision, and when — `Go back to: Split clip, 10:42`. */
export function goName(row: Pick<HistoryRow, 'state' | 'label' | 'iso'>): string {
	const t = Date.parse(row.iso);
	const time = Number.isFinite(t) ? new Date(t).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }) : '';
	return `${row.state === 'undone' ? 'Redo to' : 'Go back to'}: ${row.label}${time ? `, ${time}` : ''}`;
}

/** The notice after a move: where the cut is now and what happens to what is after it. `later` is how many revisions follow the target. */
export function wentTo(label: string, state: RevisionState, later: number): { text: string; description: string | null } {
	if (state === 'undone') return { text: `Went forward to “${label}”`, description: null };
	return {
		text: `Went back to “${label}”`,
		description: later > 0 ? `${later === 1 ? 'The later edit is' : `The ${later} later edits are`} kept only until you make a new edit.` : null
	};
}

/** How many rows the list draws: the newest `shown`, widened to take in the current one (a cut stepped far back has its
 *  marker below the fold otherwise), and how many are left out. Drawing a few hundred rows that nobody reads is what
 *  made every re-render cost the whole list. */
export function windowRows<T extends { state: RevisionState }>(rows: readonly T[], shown: number): { rows: T[]; more: number } {
	const at = rows.findIndex((r) => r.state === 'current');
	const take = Math.min(rows.length, Math.max(shown, at < 0 ? 0 : at + 3));
	return { rows: rows.slice(0, take), more: rows.length - take };
}

/** Rows drawn before "Show more". */
export const ROWS_SHOWN = 40;

/** Whether the page read has to be widened to find the head: nothing in it is current, and older revisions exist. */
export function needsOlderForHead(history: readonly Pick<Revision, 'current'>[], more: boolean): boolean {
	return more && history.length > 0 && !history.some((r) => r.current);
}
