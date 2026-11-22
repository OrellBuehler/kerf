// What the menu choices that are not registry actions do (`menus.ts`
// `MenuCommand`): pick the project frame, a track height, show or hide a panel, move
// a panel into a window of its own.
// Shared with the timeline's Frame button and the Deliver panel's Project frame, so a
// frame chosen from any of them is chosen the same way.

import { DELIVERY_PRESETS, fitLabel, fpsLabel, keepRate, presetFor, sameRate, withRate } from './delivery-formats';
import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { workspace } from './workspace.svelte';
import { popout } from './popout.svelte';
import { tick } from 'svelte';
import { toast } from './notifications.svelte';
import { findHistory } from './history-ui';
import { timelineRoot, timelineStop } from './timeline-focus';
import { setLastArea } from './focus-area';
import { windows } from './windows.svelte';
import { holdForPreview, report } from './ops';
import type { MenuCommand } from './menus';
import type { Fit } from './types';

/** Cut the project for a frame (`null`: follow the footage). Changing it
 *  reshapes the preview, the scrubbed still and the export together, so the
 *  vertical crop is something you compose against rather than discover in the
 *  rendered file. */
export async function setDeliveryPreset(id: string): Promise<void> {
	const p = DELIVERY_PRESETS.find((d) => d.id === id);
	if (!p || holdForPreview()) return;
	// The shape it already is writes nothing — no revision, no notice: a click on the chosen chip (or menu row) is not an edit,
	// and picking 9:16 again must not put the project's fit back to the preset's.
	if (presetFor(editor.timeline.format).id === id) return;
	try {
		// A different shape does not take the frame rate away: it is the project's.
		const format = keepRate(p.format, editor.timeline.format);
		await editor.setDeliveryFormat(format);
		const rate = format?.fps != null ? ` at ${fpsLabel(format.fps)} fps` : '';
		toast.success(format ? `Project frame: ${p.label}, ${fitLabel(format.fit)}${rate}` : 'Project frame follows the footage');
	} catch (err) {
		report(err);
	}
}

/** Keep the project frame and change how footage of another shape meets it. */
export async function setDeliveryFit(fit: Fit): Promise<void> {
	const format = editor.timeline.format;
	if (!format || format.fit === fit || holdForPreview()) return;
	try {
		await editor.setDeliveryFormat({ ...format, fit });
		toast.success(`Project frame: footage ${fitLabel(fit)}`);
	} catch (err) {
		report(err);
	}
}

/** Keep the project frame and change the frame rate it is made at (`null`: follow the footage) — the rate every
 *  export, the playback and the stills default to, instead of the first clip's. */
export async function setDeliveryFps(fps: number | null): Promise<void> {
	const format = editor.timeline.format;
	if (!format || sameRate(format.fps, fps) || holdForPreview()) return;
	try {
		await editor.setDeliveryFormat(withRate(format, fps));
		toast.success(fps == null ? 'Frame rate follows the footage' : `Project frame rate: ${fpsLabel(fps)} fps`);
	} catch (err) {
		report(err);
	}
}

/** Edit › Show edit history: the Agent panel brought forward and its History section scrolled into view
 *  (`history-ui.ts` finds it). A panel that was closed mounts a moment later, so the section is looked for over a
 *  few frames before giving up. */
export async function showHistory(): Promise<void> {
	workspace.show('agent');
	for (let i = 0; i < 12; i++) {
		await tick();
		await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
		const found = findHistory(document);
		if (found) {
			const calm = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
			found.scrollIntoView({ block: 'start', behavior: calm ? 'auto' : 'smooth' });
			return;
		}
	}
}

/** *Focus the timeline* (F6, View): the panel raised if it is closed or behind a tab, the keyboard moved to its one tab stop and
 *  the timeline made the area the editing keys act from — so a keyboard-only user is never stranded in a panel, a text field
 *  included. Looks in every window the editor is in (the timeline may be popped out). */
export async function focusTimeline(): Promise<void> {
	workspace.show('timeline');
	for (let i = 0; i < 12; i++) {
		await tick();
		await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
		for (const win of windows.all) {
			const root = timelineRoot(win.document);
			const stop = root ? timelineStop(root) : null;
			// (`instanceof` is this window's class; a detached window's element is another's, so ask for the method.)
			if (stop && typeof (stop as HTMLElement).focus === 'function') {
				setLastArea('editor');
				(stop as HTMLElement).focus();
				return;
			}
		}
	}
	// No timeline to land on (the panel could not be shown): the area is still where the user asked to be.
	setLastArea('editor');
}

export function runMenuCommand(c: MenuCommand): void {
	switch (c.type) {
		case 'delivery':
			void setDeliveryPreset(c.preset);
			break;
		case 'deliveryFps':
			void setDeliveryFps(c.fps);
			break;
		case 'height':
			ui.setAllHeights(c.preset);
			break;
		case 'panel':
			workspace.toggle(c.panel);
			break;
		case 'detach':
			popout.toggle(c.panel);
			break;
		case 'workspace':
			workspace.switchTo(c.key);
			break;
		// File › Open Recent needs the page's unsaved-work question; the page runs those (`onMenuCommand`).
		case 'recent':
		case 'recent-clear':
			break;
	}
}
