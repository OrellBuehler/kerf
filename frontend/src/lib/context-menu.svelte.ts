/* The app-wide custom context menu (Svelte 5 runes). Each view builds its own item
   list on right-click and calls `contextMenu.show(e, items)`. One menu is open at a
   time, and it belongs to the window it was opened in: the <ContextMenu /> in
   +page.svelte draws it in the editor window, and one mounted in each detached
   window (`popout.svelte.ts`) draws it there — a menu has to be in the document of
   the window that was clicked, or it appears on the wrong screen. */

import { windowOf } from './realm';

export type MenuItem =
	| {
			type?: 'item';
			label: string;
			icon?: string;
			shortcut?: string;
			danger?: boolean;
			disabled?: boolean;
			/** Why a disabled item is disabled, said under its label — an item that cannot be
			 *  used and does not say why is a dead end. */
			reason?: string;
			action: () => void;
	  }
	| { type: 'separator' }
	/** A non-interactive title row: what the menu is about. */
	| { type: 'header'; label: string; sub?: string }
	/** A non-interactive fact row (`label` left, `value` right, mono). */
	| { type: 'info'; label: string; value: string; title?: string };

class ContextMenuState {
	visible = $state(false);
	x = $state(0);
	y = $state(0);
	items = $state<MenuItem[]>([]);
	/** The window it is open in. */
	win = $state<Window | null>(null);

	/** Open the menu at the pointer, suppressing the native browser menu. The window is
	 *  the one the event happened in; a menu opened from a synthetic event (one built to
	 *  anchor a menu under a button) names the button as `anchor` instead. */
	show(e: MouseEvent, items: MenuItem[], anchor?: Node | null) {
		e.preventDefault();
		e.stopPropagation();
		const target = e.target;
		const node = target && 'nodeType' in target ? (target as Node) : anchor;
		this.items = items;
		this.x = e.clientX;
		this.y = e.clientY;
		this.win = windowOf(node);
		this.visible = true;
	}

	close() {
		this.visible = false;
		this.items = [];
		this.win = null;
	}
}

export const contextMenu = new ContextMenuState();
