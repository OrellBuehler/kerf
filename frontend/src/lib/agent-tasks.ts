// What the agent queue's cards do. A ready task whose edits are staged is reviewed in the
// review card above the queue — one Apply, which asks about a stale cut and says what it did —
// so its own card has no buttons; any other ready task is just marked done or dropped, and
// when the backend refuses that is said instead of vanishing into a click handler.

import { agent } from './agent.svelte';
import { confirmAction } from './api';
import { discardProposalCopy } from './confirm-copy';
import { attempt } from './ops';
import { discardNeedsConfirm } from './proposal-ui';
import { editor } from './state.svelte';
import type { StagedEdit, Task } from './types';

export type ReadyCard = 'reviewing' | 'resolve';

/** What a ready task's card offers: nothing (`reviewing`: its proposal is in the review card)
 *  or Apply / Dismiss (`resolve`). */
export function readyCard(task: Pick<Task, 'id'>, staged: Pick<StagedEdit, 'task_id'> | null): ReadyCard {
	return staged?.task_id === task.id ? 'reviewing' : 'resolve';
}

/** Mark a ready task done. Reports a refusal as an error toast; never throws. */
export const resolveReady = (taskId: string): Promise<void> => attempt(() => agent.resolve(taskId));

/** Drop a task from the queue — which discards the proposal staged under it, so one of more than a few
 *  changes asks first (the review card's Discard does too). Reports a refusal as an error toast; never
 *  throws. */
export async function dismissTask(taskId: string): Promise<void> {
	const staged = editor.staged;
	if (staged?.task_id === taskId && discardNeedsConfirm(staged.diff.entries.length)) {
		const copy = discardProposalCopy(staged.diff.entries.length);
		if (!(await confirmAction(copy.message, copy.title, copy))) return;
	}
	await attempt(() => agent.remove(taskId));
}
