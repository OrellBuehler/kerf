/* Whether a toast's Undo may still take back the edit the toast was about — pure.
 *
 * A toast outlives its edit, and revision numbers are not an identity: recording an edit drops the redo
 * branch and takes `head + 1` (`Project::record_revision`), so edit → Undo → another edit gives the new edit the
 * old one's `seq`. A revision is therefore remembered as its `seq` *and* its `created_at`; one without the
 * other can be a different edit that happens to stand in the same place. */

import type { Revision } from './types';

/** The revision an edit left the project at. */
export interface RevisionStamp {
	seq: number;
	at: string;
}

type Entry = Pick<Revision, 'seq' | 'created_at' | 'current'>;

/** The stamp of the current revision, or `undefined` where the history cannot say (not loaded). */
export function headStamp(history: readonly Entry[]): RevisionStamp | undefined {
	const r = history.find((x) => x.current);
	return r ? { seq: r.seq, at: r.created_at } : undefined;
}

/**
 * What a click on the toast's Undo should do now, given the stamp taken just after its edit:
 *  - `undo`: the edit is the latest (it is the current revision) — take it back;
 *  - `undone`: it is already undone, waiting in the redo branch or replaced by an edit made since — there is nothing of it to take back;
 *  - `moved-on`: later edits sit on top of it, so an Undo would take back one of those instead.
 * `history` is the newest page of revisions: one that is not in it and is older than the page has edits after it.
 */
export function undoVerdict(at: RevisionStamp | undefined, history: readonly Entry[]): 'undo' | 'undone' | 'moved-on' {
	const head = history.find((x) => x.current);
	if (!at || !head) return !at && !head ? 'undo' : 'moved-on';
	if (head.seq === at.seq && head.created_at === at.at) return 'undo';
	const mine = history.find((x) => x.seq === at.seq && x.created_at === at.at);
	if (mine) return head.seq > mine.seq ? 'moved-on' : 'undone';
	// Gone: the redo branch it sat in was dropped by a newer edit (that one may even carry its number) ...
	if (at.seq >= head.seq || history.some((x) => x.seq === at.seq)) return 'undone';
	// ... or it is older than the page of history held.
	return 'moved-on';
}
