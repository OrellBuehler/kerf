// Taking media out of the project and pointing it at another file — what the bin's menu, the Inspector and the
// Relink dialog all do, so each asks the same questions and says the same thing afterwards.

import { checkRelink, confirmAction, getStagedTimeline, pickRelinkPath } from './api';
import { relinkDropsAnalysisCopy, removeMediaCopy } from './confirm-copy';
import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { mediaStatus } from './media-status.svelte';
import { toast } from './notifications.svelte';
import { holdForPreview, report } from './ops';
import { thumbnails } from './thumbnails';
import { clipsUsing, relinkLine, removedLine } from './library-copy';
import type { Asset } from './types';

/** Take `asset` out of the project. Unused media is a plain question; media a clip (or the proposal) uses names
 *  what goes with it first. Resolves to whether it went. */
export async function removeMediaFlow(asset: Asset): Promise<boolean> {
	if (holdForPreview()) return false;
	const live = clipsUsing(editor.liveTimeline, asset.id);
	let proposed = 0;
	if (editor.staged) {
		try {
			proposed = clipsUsing(await getStagedTimeline(), asset.id);
		} catch {
			/* the backend judges it again below */
		}
	}
	const copy = removeMediaCopy(asset.name, live, proposed > 0);
	if (!(await confirmAction(copy.message, copy.title, copy))) return false;
	try {
		const done = await editor.removeMedia(asset.id, live > 0 || proposed > 0);
		toast.success(removedLine(asset.name, done.clips_removed));
		void mediaStatus.refresh();
		return true;
	} catch (e) {
		report(e);
		return false;
	}
}

/** Pick the file `asset` has moved to and link it. A file that is a different recording of it drops the asset's
 *  analysis, which is asked about first (the backend's dry run says so, and refuses a file that cannot stand in
 *  for it before anything is asked). Resolves to whether it was relinked. */
export async function relinkMediaFlow(asset: Pick<Asset, 'id' | 'name'>): Promise<boolean> {
	const path = await pickRelinkPath(asset.name);
	if (!path) return false;
	try {
		const check = await checkRelink(asset.id, path);
		if (check.has_analysis && !check.analysis_kept) {
			const c = relinkDropsAnalysisCopy(asset.name);
			if (!(await confirmAction(c.message, c.title, c))) return false;
		}
		const done = await editor.relinkMedia(asset.id, path);
		thumbnails.forget(asset.id);
		await mediaStatus.refresh();
		ui.refreshPreview();
		toast.success(relinkLine(asset.name, done));
		return true;
	} catch (e) {
		report(e);
		return false;
	}
}

/** A project has come up: if some of its media is gone, say so and open the dialog that fixes it. */
export async function checkMissingOnOpen(): Promise<void> {
	await mediaStatus.refreshMissing();
	if (mediaStatus.missingList.length > 0) ui.openRelink();
}
