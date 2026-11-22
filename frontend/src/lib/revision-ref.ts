/* Which revision an action produced — pure. The history numbers its revisions one after another, and after an undo and a
 * new edit the backend gives a *different* revision the same number (`record_revision` deletes everything after the head
 * and inserts at head + 1), so a `seq` alone does not name a revision for as long as a toast's Undo button stays on screen.
 * A revision is named by its number and the time it was made: that pair is not recycled. An Undo button that captured one
 * acts only while that revision is still the head, and otherwise says the cut has been edited since — it never undoes
 * whatever is newest by then. */

import type { Revision } from './types';

export interface RevisionRef {
	seq: number;
	created_at: string;
}

type Stamped = Pick<Revision, 'seq' | 'created_at' | 'current'>;

/** The revision the cut is at, or `null` when the history held does not say (a head older than the page read). */
export function headRef(history: readonly Stamped[]): RevisionRef | null {
	const head = history.find((r) => r.current);
	return head ? { seq: head.seq, created_at: head.created_at } : null;
}

export const sameRevision = (a: RevisionRef | null | undefined, b: RevisionRef | null | undefined): boolean =>
	!!a && !!b && a.seq === b.seq && a.created_at === b.created_at;

/** Whether the history still holds exactly this revision (not another one at its number). */
export const holdsRevision = (history: readonly Stamped[], ref: RevisionRef | null | undefined): boolean =>
	!!ref && history.some((r) => r.seq === ref.seq && r.created_at === ref.created_at);

/** What an Undo that captured `ref` may do now: `undo` while `ref` is the head, `edited` when the head is another
 *  revision, `unknown` when the head cannot be told (then nothing is done either). */
export function undoVerdict(ref: RevisionRef, history: readonly Stamped[]): 'undo' | 'edited' | 'unknown' {
	const head = headRef(history);
	if (!head) return 'unknown';
	return sameRevision(head, ref) ? 'undo' : 'edited';
}

/** What an Undo that declines to act says. */
export const EDITED_SINCE = 'The cut has been edited since, so Undo here would undo something else. Use Edit › Undo, or History.';
