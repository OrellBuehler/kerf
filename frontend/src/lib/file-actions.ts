// File-menu commands that are more than a call to the backend: they ask where to
// put the result and say how it went. Shared by the menu, the shortcut table and
// the preview's own context menu, so each of them does exactly the same thing.

import { exportCover, inTauri, pickCoverPath, revealPath } from './api';
import { toast } from './notifications.svelte';
import { ui } from './editor-ui.svelte';

/** Write the frame under the playhead as a cover image — the thumbnail a
 *  platform shows before anyone presses play. Rendered at the full delivery
 *  frame from the original media, so it is the picture people actually see,
 *  not the downscaled preview on screen. */
export async function saveCoverFrame(): Promise<void> {
	if (!inTauri()) {
		toast.info('Cover frames are rendered with FFmpeg in the desktop app.');
		return;
	}
	const path = await pickCoverPath();
	if (!path) return;
	try {
		const out = await exportCover(ui.time, path);
		toast.success(`Cover saved → ${out}`, {
			action: { label: 'Show in folder', onClick: () => void revealPath(out).catch(() => {}) }
		});
	} catch (e) {
		toast.error(e instanceof Error ? e.message : String(e));
	}
}
