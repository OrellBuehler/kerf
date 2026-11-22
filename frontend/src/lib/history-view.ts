/* The edit history as the Agent panel's History section draws it — pure. The revisions come newest first from
 * `editor.history`; what the section adds is who made each one, when, and where it stands against the head: the revision
 * the cut is at is *current*, the ones before it are *past*, and the ones after it are *undone* — still in the project,
 * redoable, until a new edit drops them (`record_revision` deletes everything after the head). Going back is therefore not
 * a loss and asks nothing; what loses work is the edit that follows, and the section says so while it is true. */

import type { EditSource, Revision } from './types';

export type RevisionState = 'current' | 'past' | 'undone';

export interface HistoryRow {
	seq: number;
	label: string;
	source: EditSource;
	/** Who made it, as the row says: `You`, `Agent`, `Kerf` (the baseline and what the app itself did). */
	who: string;
	/** How long ago, quietly (`just now`, `4 min ago`); the exact time is `at`. */
	when: string;
	/** The exact time, for the tooltip over `when`. */
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
export function historyRows(revisions: readonly Revision[], now: number): HistoryRow[] {
	const head = revisions.find((r) => r.current)?.seq;
	return [...revisions]
		.sort((a, b) => b.seq - a.seq)
		.map((r) => ({
			seq: r.seq,
			label: r.label,
			source: r.source,
			who: WHO[r.source] ?? 'Kerf',
			when: relativeTime(r.created_at, now),
			at: absoluteTime(r.created_at),
			state: r.current ? 'current' : head !== undefined && r.seq > head ? 'undone' : head === undefined ? 'undone' : 'past'
		}));
}

/** How many revisions are after the one the cut is at: what redo would walk through, and a new edit would drop. */
export function undoneCount(revisions: readonly Pick<Revision, 'current' | 'seq'>[]): number {
	const head = revisions.find((r) => r.current)?.seq;
	return head === undefined ? 0 : revisions.filter((r) => r.seq > head).length;
}

const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? '' : 's'}`;

/** The note over the list while the cut is not at the newest revision. */
export function backNote(undone: number): string | null {
	if (undone <= 0) return null;
	return `You are ${plural(undone, 'edit')} back. The ${undone === 1 ? 'edit' : `${undone} edits`} after this point ${undone === 1 ? 'is' : 'are'} kept until you make a new edit — a new edit drops ${undone === 1 ? 'it' : 'them'}.`;
}

/** What a row's button says: it is a move of the head, back (`past`) or forward (`undone`). */
export function goLabel(state: RevisionState): string {
	return state === 'undone' ? 'Redo to here' : 'Go back';
}

export function goTip(state: RevisionState, later: number): string {
	if (state === 'undone') return 'Move the cut forward to this edit';
	return later > 0
		? `Put the cut back as it was at this edit. The ${plural(later, 'edit')} after it ${later === 1 ? 'is' : 'are'} kept until you make a new edit.`
		: 'Put the cut back as it was at this edit';
}

/** The notice after a move: where the cut is now and what happened to what is after it. `later` is how many revisions follow the target. */
export function wentTo(label: string, state: RevisionState, later: number): { text: string; description: string | null } {
	if (state === 'undone') return { text: `Went forward to “${label}”`, description: null };
	return {
		text: `Went back to “${label}”`,
		description: later > 0 ? `${later === 1 ? 'The later edit is' : `The ${later} later edits are`} kept until you make a new edit.` : null
	};
}
