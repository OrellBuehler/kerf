/* The dockable workspace (Svelte 5 runes). `Workspace.svelte` owns the dockview
   instance and attaches it here; the toolbar's Panels menu drives it. */

import type { DockviewApi } from 'dockview';
import { DEFAULT_LAYOUT, PANELS, type PanelId } from './layout';

class WorkspaceState {
	/** The panels currently shown, in dockview's order. */
	open = $state<PanelId[]>([]);
	#api: DockviewApi | null = null;

	attach(api: DockviewApi) {
		this.#api = api;
		const sync = () => (this.open = api.panels.map((p) => p.id as PanelId));
		sync();
		api.onDidAddPanel(sync);
		api.onDidRemovePanel(sync);
	}

	detach() {
		this.#api = null;
		this.open = [];
	}

	isOpen(id: PanelId) {
		return this.open.includes(id);
	}

	/** Bring a panel to the front, opening it beside the active group when it
	 *  is closed. */
	show(id: PanelId) {
		const api = this.#api;
		if (!api) return;
		const existing = api.getPanel(id);
		if (existing) {
			existing.api.setActive();
			return;
		}
		const spec = PANELS[id];
		const group = api.activeGroup;
		api.addPanel({
			id,
			component: id,
			title: spec.title,
			minimumWidth: spec.minimumWidth,
			minimumHeight: spec.minimumHeight,
			position: group ? { referenceGroup: group, direction: 'right' } : undefined
		});
	}

	hide(id: PanelId) {
		this.#api?.getPanel(id)?.api.close();
	}

	toggle(id: PanelId) {
		if (this.isOpen(id)) this.hide(id);
		else this.show(id);
	}

	reset() {
		this.#api?.fromJSON(structuredClone(DEFAULT_LAYOUT));
	}
}

export const workspace = new WorkspaceState();
