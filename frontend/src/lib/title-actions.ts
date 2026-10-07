// What the Titles controls do, shared by the Inspector's "Titles lane" section
// and the library's Titles tab so the two cannot drift: add a title at the
// playhead, add one in a preset style, caption the cut, pick a title to edit.

import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { toast } from './notifications.svelte';
import { attempt } from './ops';
import type { TextStyle } from './style-presets';
import type { TextOverlay } from './types';

/** A plain title at the playhead. */
export function addTextHere(): Promise<void> {
	const at = Math.max(0, ui.time);
	return attempt(() => editor.addTitle('Text', at, at + 3));
}

/** Add a preset-styled overlay at the playhead: create, style, then (for faded
 *  styles) keyframe the opacity in and out, and select it. */
export function addStyledTitle(s: TextStyle): Promise<void> {
	const at = Math.max(0, ui.time);
	return attempt(async () => {
		await editor.addOverlay(s.text, at, at + s.duration);
		const created = editor.overlays[editor.overlays.length - 1];
		if (!created) return;
		const { bg, ...rest } = s.style;
		await editor.updateOverlay(created.id, bg == null ? rest : { ...rest, bg });
		if (s.fade > 0) {
			const kf = (time: number, opacity: number) => ({ time, pos_x: s.style.pos_x, pos_y: s.style.pos_y, opacity });
			await editor.setOverlayKeyframes(created.id, [
				kf(0, 0),
				kf(s.fade, 1),
				kf(s.duration - s.fade, 1),
				kf(s.duration, 0)
			]);
		}
		editor.selectOverlay(created.id);
	});
}

/** Pick a title from the lane's list, and bring the playhead into its span so
 *  the preview has it on screen to move and resize. Picking the selected one
 *  again lets go of it. */
export function pickTitle(o: TextOverlay) {
	if (editor.selectedOverlayId === o.id) {
		editor.selectOverlay(null);
		return;
	}
	editor.selectOverlay(o.id);
	if (ui.time < o.start || ui.time > o.end) ui.seek(o.start);
}

/** Captions are placed in timeline time, so they follow the cut — which also
 *  means a later trim moves the words out from under them. Re-running replaces
 *  the generated set, so the button stays the same after the first press and
 *  only its label admits what it is doing. */
export function makeCaptions(): Promise<void> | undefined {
	if (!editor.timeline.tracks.some((t) => t.clips.length > 0)) {
		toast.error('Put a clip on the timeline first');
		return;
	}
	return attempt(() => editor.generateCaptions({ style: ui.captionStyle }));
}

export function dropCaptions(): Promise<void> {
	return attempt(() => editor.clearCaptions());
}
