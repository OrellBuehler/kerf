/* How the grade's controls write: one edit, built from the clip the gesture *began* on, as the cut has it when the edit is
 * made. A grade is one value (`set_grade` replaces curves, wheels and LUT together), so a write that merges its change into
 * the grade of whichever clip is selected by then — a key run written when focus leaves, which is when the click that selects
 * another clip lands — would copy that clip's curves into this one. And two writes a moment apart (a puck put down, then
 * another) must not each read the grade before the other has landed: they go one after the other, each reading when its turn comes. */

import type { Grade } from './types';

export interface GradeHost {
	/** The grade of a clip as the cut has it now, or `undefined` when the clip is gone. */
	gradeOf(clipId: string): Grade | undefined;
	/** Replace a clip's grade (one edit). */
	set(clipId: string, grade: Grade): Promise<unknown>;
	/** A refusal or failure of an edit. */
	report(error: unknown): void;
}

/** What a change does to a grade: the grade it makes, or `null` for no change (nothing is written). */
export type GradeChange = (grade: Grade) => Grade | null;

/** A writer over `host`: `write(clipId, change)` queues the change behind the ones before it and resolves once it has been
 *  written, skipped (no change, no clip) or refused (reported). It never rejects. */
export function gradeWriter(host: GradeHost): (clipId: string, change: GradeChange) => Promise<void> {
	let tail: Promise<void> = Promise.resolve();
	return (clipId, change) => {
		const turn = async () => {
			try {
				const now = host.gradeOf(clipId);
				if (!now) return;
				const next = change(now);
				if (next) await host.set(clipId, next);
			} catch (e) {
				host.report(e);
			}
		};
		tail = tail.then(turn, turn);
		return tail;
	};
}
