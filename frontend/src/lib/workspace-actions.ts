// What the workspace-wide resets do once asked — Window › Reset all workspaces and Settings › Advanced share
// them, so both ask the same question and say the same thing afterwards.

import { confirmAction } from './api';
import { CONFIRM } from './confirm-copy';
import { settings } from './settings.svelte';
import { workspace } from './workspace.svelte';

/** Whether any workspace has an arrangement (or a library tab) of its own — something a reset would throw away. */
export function hasStoredArrangement(w: { layouts: Record<string, unknown>; library: { tabs: Record<string, unknown> } }): boolean {
	return Object.keys(w.layouts).length > 0 || Object.keys(w.library.tabs).length > 0;
}

/** Every workspace back to its preset. It throws away every arrangement made, so it asks first — unless there is
 *  none to lose, when it only rebuilds the one on screen. Resolves to whether it went ahead. */
export async function resetAllWorkspacesAsked(): Promise<boolean> {
	const reset = CONFIRM.resetWorkspaces;
	if (hasStoredArrangement(settings.workspaces) && !(await confirmAction(reset.message, reset.title, reset))) return false;
	workspace.resetAll();
	return true;
}
