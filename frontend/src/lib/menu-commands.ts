// What the menu choices that are not registry actions do (`menus.ts`
// `MenuCommand`): pick a delivery frame, a track height, show or hide a panel, move
// a panel into a window of its own.
// Shared with the timeline's own delivery picker, so a frame chosen from either
// is chosen the same way.

import { DELIVERY_PRESETS, fitLabel } from './delivery-formats';
import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { workspace } from './workspace.svelte';
import { popout } from './popout.svelte';
import { toast } from './notifications.svelte';
import type { MenuCommand } from './menus';

/** Cut the project for a delivery frame (`null`: follow the footage). Changing it
 *  reshapes the preview, the scrubbed still and the export together, so the
 *  vertical crop is something you compose against rather than discover in the
 *  rendered file. */
export async function setDeliveryPreset(id: string): Promise<void> {
	const p = DELIVERY_PRESETS.find((d) => d.id === id);
	if (!p) return;
	try {
		await editor.setDeliveryFormat(p.format);
		toast.success(p.format ? `Cutting for ${p.label} (${fitLabel(p.format.fit)})` : 'Following the footage');
	} catch (err) {
		toast.error(err instanceof Error ? err.message : String(err));
	}
}

export function runMenuCommand(c: MenuCommand): void {
	switch (c.type) {
		case 'delivery':
			void setDeliveryPreset(c.preset);
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
	}
}
