// What the workspace-wide resets do once asked — Window › Reset all workspaces and Settings › Advanced share
// them, so both ask the same question and say the same thing afterwards.

import { confirmAction } from './api';
import { CONFIRM, deleteWorkspaceCopy } from './confirm-copy';
import { settings } from './settings.svelte';
import { toast } from './notifications.svelte';
import { workspace } from './workspace.svelte';
import { hasStoredArrangement, isBuiltinKey, workspaceLabel, type WorkspaceKey } from './workspaces';

/** Every workspace back to its preset. It throws away every arrangement made, so it asks first — unless there is
 *  none to lose, when it only rebuilds the one on screen. Resolves to whether it went ahead. */
export async function resetAllWorkspacesAsked(): Promise<boolean> {
	const reset = CONFIRM.resetWorkspaces;
	if (hasStoredArrangement(settings.workspaces) && !(await confirmAction(reset.message, reset.title, reset))) return false;
	workspace.resetAll();
	return true;
}

/** Delete a workspace the user saved — after asking, because its arrangement goes with it. Resolves to whether it went ahead. */
export async function deleteWorkspaceAsked(key: WorkspaceKey): Promise<boolean> {
	const name = workspaceLabel(settings.workspaces, key);
	if (isBuiltinKey(key)) {
		toast.info(`${name} comes with Kerf, so it is reset rather than deleted`);
		return false;
	}
	const copy = deleteWorkspaceCopy(name);
	if (!(await confirmAction(copy.message, copy.title, copy))) return false;
	workspace.remove(key);
	return true;
}
