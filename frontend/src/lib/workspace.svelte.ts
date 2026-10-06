/* The dockable workspace (Svelte 5 runes). `Workspace.svelte` owns the dockview
   instance and attaches it here; the title bar's workspace tabs and the
   toolbar's Panels menu drive it.

   A workspace is an arrangement of panels, nothing else. Switching swaps the
   dockview layout and leaves the project alone — the cut, the selection, the
   playhead and playback are `editor` / `ui` state and are not touched. (Panels
   are rebuilt by dockview when the layout is replaced, so a panel's own scroll
   position starts over; anything that matters lives in those singletons.) */

import type { DockviewApi } from 'dockview';
import { LIBRARY_RAIL_WIDTH, PANELS, presetLayout, type PanelId } from './layout';
import type { SerializedDockview } from 'dockview';
import { layoutFor, shouldPersistLayout, type WorkspaceId } from './workspaces';
import { settings } from './settings.svelte';

/** Where a panel opens when it is brought back from the Panels menu: the
 *  library on the preview's left and the deliver panel on its right — that is
 *  where the presets put them — everything else beside whatever is active. */
const BESIDE_PREVIEW: Partial<Record<PanelId, 'left' | 'right'>> = { library: 'left', deliver: 'right' };

/** How long after the last layout change the arrangement is written. */
const SAVE_DELAY_MS = 500;

class WorkspaceState {
	/** The panels currently shown, in dockview's order. */
	open = $state<PanelId[]>([]);
	/** The workspace on screen. */
	active = $state<WorkspaceId>('edit');
	/** Whether a dock is attached; until then the title bar's tabs show the saved
	 *  workspace rather than the default. */
	attached = $state(false);
	#api: DockviewApi | null = null;
	#host: HTMLElement | null = null;
	#timer: ReturnType<typeof setTimeout> | null = null;
	/** True while the dock is being rebuilt from a layout: the events that
	 *  produces are not the user rearranging anything. */
	#restoring = false;
	/** What restoring the active workspace would give back: the layout as it
	 *  settled once the workspace was built, or the one last written. `null`
	 *  while it is still settling. A change is only written if it differs from
	 *  this (see `shouldPersistLayout`) — dockview reports layout changes for a
	 *  great deal that is not a rearrangement, and writing each would mark every
	 *  workspace merely visited as customised. */
	#reference: SerializedDockview | null = null;
	#settle = 0;
	#subs: Array<{ dispose(): void }> = [];

	/** Take over a freshly created dock (built in `host`): build the active
	 *  workspace's layout and start saving changes to it. */
	attach(api: DockviewApi, host: HTMLElement) {
		this.#api = api;
		this.#host = host;
		this.active = settings.workspaces.active;
		this.attached = true;
		this.#restore(this.active);
		const sync = () => (this.open = api.panels.map((p) => p.id as PanelId));
		sync();
		this.#subs.push(
			api.onDidAddPanel(sync),
			api.onDidRemovePanel(sync),
			api.onDidLayoutChange(() => {
				if (this.#restoring || !this.#reference) return;
				if (this.#timer) clearTimeout(this.#timer);
				this.#timer = setTimeout(() => this.#save(), SAVE_DELAY_MS);
			})
		);
	}

	detach() {
		this.#flush();
		for (const s of this.#subs.splice(0)) s.dispose();
		this.#api = null;
		this.#host = null;
		this.attached = false;
		this.open = [];
	}

	/** The workspace to show as chosen: the live one once the dock is up, the
	 *  saved one before that (the tabs render before the settings have loaded). */
	get shown(): WorkspaceId {
		return this.attached ? this.active : settings.workspaces.active;
	}

	/** Build `id`'s layout — the user's arrangement or its preset. A layout
	 *  dockview rejects falls back to the preset, and then the dock is still
	 *  something the editor can be used in. */
	#restore(id: WorkspaceId) {
		const api = this.#api;
		if (!api) return;
		this.#restoring = true;
		try {
			try {
				api.fromJSON(layoutFor(settings.workspaces, id));
			} catch (e) {
				console.error('could not restore the layout', e);
				api.fromJSON(presetLayout(id));
			}
			// A layout is built at the size it was saved at; the dock only learns its
			// real size a frame or two later, off a ResizeObserver. A panel that
			// changes a constraint in that gap (the library does, folded) makes
			// dockview re-split the whole grid evenly and every size is lost — so
			// measure now, before any panel gets the chance.
			const host = this.#host;
			if (host && host.clientWidth > 0 && host.clientHeight > 0) api.layout(host.clientWidth, host.clientHeight, true);
		} finally {
			this.#restoring = false;
		}
		this.open = api.panels.map((p) => p.id as PanelId);
		this.#takeReference();
	}

	/** Wait for the layout to stop moving, then call it the baseline. The panels
	 *  that size their own group (the library, folded or unfolded) do it a
	 *  microtask after they mount, and dockview reports layout changes through a
	 *  microtask-buffered event; two frames covers both. */
	#takeReference() {
		this.#reference = null;
		const settle = ++this.#settle;
		requestAnimationFrame(() =>
			requestAnimationFrame(() => {
				if (settle === this.#settle && this.#api) this.#reference = this.#api.toJSON();
			})
		);
	}

	/** The library is about to fold or unfold. Whatever the user changed before
	 *  that is still on the debounce; write it now, while the layout still shows
	 *  it and nothing of the fold does. */
	beforeLibraryMove() {
		this.#flush();
	}

	/** The library folded or unfolded: widths moved, but that is what the
	 *  `collapsed` setting already records, not a rearrangement to write down.
	 *  The layout events the move caused are dropped and the new geometry becomes
	 *  the reference. */
	afterLibraryMove() {
		if (!this.#api) return;
		if (this.#timer) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
		this.#takeReference();
	}

	/** Remember how the active workspace is arranged now — if it is arranged any
	 *  differently from how it came back. */
	#save() {
		this.#timer = null;
		const api = this.#api;
		if (!api) return;
		const layout = api.toJSON();
		const id = this.active;
		const hasEntry = id in settings.workspaces.layouts;
		if (!shouldPersistLayout(layout, this.#reference, presetLayout(id), hasEntry)) return;
		this.#reference = layout;
		settings.saveWorkspaceLayout(id, layout);
	}

	/** Write out a change still waiting on the debounce. */
	#flush() {
		if (!this.#timer) return;
		clearTimeout(this.#timer);
		this.#save();
	}

	/** Swap to another workspace. The arrangement being left is kept first if it
	 *  was changed; the library shows the tab that workspace last had. */
	switchTo(id: WorkspaceId) {
		if (!this.#api || id === this.active) return;
		this.#flush();
		this.active = id;
		settings.setActiveWorkspace(id);
		this.#restore(id);
	}

	isOpen(id: PanelId) {
		return this.open.includes(id);
	}

	/** Bring a panel to the front, opening it when it is closed. */
	show(id: PanelId) {
		const api = this.#api;
		if (!api) return;
		const existing = api.getPanel(id);
		if (existing) {
			existing.api.setActive();
			return;
		}
		const spec = PANELS[id];
		const side = BESIDE_PREVIEW[id];
		// Beside the preview when it is open (the row of panels, not the whole
		// layout — a root-level split would cut the full-width timeline short).
		const anchor = side ? (api.getPanel('preview')?.group ?? api.activeGroup) : api.activeGroup;
		const width = id === 'library' && settings.libraryCollapsed ? LIBRARY_RAIL_WIDTH : spec.defaultWidth;
		const anchored = anchor?.api.width ?? 0;
		api.addPanel({
			id,
			component: id,
			title: spec.title,
			minimumWidth: spec.minimumWidth,
			minimumHeight: spec.minimumHeight,
			initialWidth: width,
			position: anchor ? { referenceGroup: anchor, direction: side ?? 'right' } : side ? { direction: side } : undefined
		});
		// dockview takes a new group's width from the last group in the row; the
		// panel it opened beside is the one that should give it up.
		if (side && anchor && width && anchored) anchor.api.setSize({ width: Math.max(anchored - width, anchor.minimumWidth) });
	}

	hide(id: PanelId) {
		this.#api?.getPanel(id)?.api.close();
	}

	toggle(id: PanelId) {
		if (this.isOpen(id)) this.hide(id);
		else this.show(id);
	}

	/** Put the active workspace back to its preset, forgetting what was saved. */
	reset() {
		if (!this.#api) return;
		if (this.#timer) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
		// No entry, and none written back: what settles is the preset, which is
		// what the baseline will be.
		settings.clearWorkspaceLayout(this.active);
		this.#restore(this.active);
	}
}

export const workspace = new WorkspaceState();
