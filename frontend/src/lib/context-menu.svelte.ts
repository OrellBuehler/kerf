/* A single, app-wide custom context menu (Svelte 5 runes). Each view builds its
   own item list on right-click and calls `contextMenu.show(e, items)`; the lone
   <ContextMenu /> instance in +page.svelte renders whatever is open. */

export type MenuItem =
	| {
			type?: 'item';
			label: string;
			icon?: string;
			shortcut?: string;
			danger?: boolean;
			disabled?: boolean;
			action: () => void;
	  }
	| { type: 'separator' };

class ContextMenuState {
	visible = $state(false);
	x = $state(0);
	y = $state(0);
	items = $state<MenuItem[]>([]);

	/** Open the menu at the pointer, suppressing the native browser menu. */
	show(e: MouseEvent, items: MenuItem[]) {
		e.preventDefault();
		e.stopPropagation();
		this.items = items;
		this.x = e.clientX;
		this.y = e.clientY;
		this.visible = true;
	}

	close() {
		this.visible = false;
		this.items = [];
	}
}

export const contextMenu = new ContextMenuState();
