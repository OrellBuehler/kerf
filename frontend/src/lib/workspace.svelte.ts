/* The dockable workspace (Svelte 5 runes). `Workspace.svelte` owns the dockview
   instance and attaches it here; the title bar's workspace tabs and the
   Window menu drive it.

   A workspace is an arrangement of panels, nothing else. Switching swaps the
   dockview layout and leaves the project alone — the cut, the selection, the
   playhead and playback are `editor` / `ui` state and are not touched. (Panels
   are rebuilt by dockview when the layout is replaced, so a panel's own scroll
   position starts over; anything that matters lives in those singletons.) */

import type { DockviewApi } from 'dockview';
import { LIBRARY_RAIL_WIDTH, PANELS, presetLayout, sameArrangement, type PanelId } from './layout';
import type { SerializedDockview } from 'dockview';
import { layoutFor, shouldPersistLayout, workspaceSpec, type WorkspaceId } from './workspaces';
import { settings } from './settings.svelte';
import { toast } from './notifications.svelte';
import { popout } from './popout.svelte';

/** Where a panel opens when it is brought back from the Window menu: the
 *  library on the preview's left and the deliver panel and the mixer on its
 *  right — that is where the presets put them — everything else beside whatever
 *  is active. */
const BESIDE_PREVIEW: Partial<Record<PanelId, 'left' | 'right'>> = { library: 'left', deliver: 'right', mixer: 'right' };

/** How long after the last layout change the arrangement is written. */
const SAVE_DELAY_MS = 500;

/** The longest a restored workspace waits for its detached windows to open before it
 *  takes the layout as it is. A window that never loads would otherwise leave the dock
 *  unable to save anything. */
const POPOUT_RESTORE_MS = 8000;

/** How long the window must hold still before a resize counts as over. Dockview
 *  re-lays the grid out a frame after each size it is given, so this is a few
 *  frames of margin. */
const RESIZE_SETTLE_MS = 150;

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
	/** Counts restores that wait on windows, so a restore that was overtaken (another
	 *  workspace was chosen meanwhile) does nothing when its windows are announced. */
	#restoreToken = 0;
	#resizing = false;
	#resizeTimer: ReturnType<typeof setTimeout> | null = null;
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
		const changed = () => {
			if (this.#restoring || !this.#reference) return;
			if (this.#timer) clearTimeout(this.#timer);
			this.#timer = setTimeout(() => this.#save(), SAVE_DELAY_MS);
		};
		this.#subs.push(
			api.onDidAddPanel(sync),
			api.onDidRemovePanel(sync),
			api.onDidLayoutChange(changed),
			// A window the user moved or resized is no layout change to dockview, but it is
			// where the window will open next time.
			api.onDidPopoutGroupPositionChange(changed),
			api.onDidPopoutGroupSizeChange(changed),
			this.#watchResize(host)
		);
	}

	/** A window resize changes more than pixels: where a group's minimum binds,
	 *  the branch's shares move too, so the layout afterwards is not the layout
	 *  the reference describes — and the next unrelated layout event (a click on
	 *  a tab) would write it down as an arrangement nobody made. So when the host
	 *  changes size: write what the user did before it (still on the debounce),
	 *  ignore the layout events the resize causes, and once it has held still
	 *  take the new layout as the reference. */
	#watchResize(host: HTMLElement): { dispose(): void } {
		let width = host.clientWidth;
		let height = host.clientHeight;
		const observer = new ResizeObserver(() => {
			// The observer reports the size it starts with; only a change is a resize.
			if (host.clientWidth === width && host.clientHeight === height) return;
			width = host.clientWidth;
			height = host.clientHeight;
			if (!this.#resizing) {
				this.#resizing = true;
				this.#flush();
			}
			this.#reference = null;
			if (this.#timer) {
				clearTimeout(this.#timer);
				this.#timer = null;
			}
			if (this.#resizeTimer) clearTimeout(this.#resizeTimer);
			this.#resizeTimer = setTimeout(() => {
				this.#resizeTimer = null;
				this.#resizing = false;
				this.#takeReference();
			}, RESIZE_SETTLE_MS);
		});
		observer.observe(host);
		return {
			dispose: () => {
				observer.disconnect();
				if (this.#resizeTimer) clearTimeout(this.#resizeTimer);
				this.#resizeTimer = null;
				this.#resizing = false;
			}
		};
	}

	detach() {
		this.#restoreToken++;
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
	 *  something the editor can be used in.
	 *
	 *  A layout with detached panels in it opens a window for each. The desktop app
	 *  only answers a `window.open` that was announced (`popout.rs`), and dockview
	 *  restores a window from a timer with nothing to say which one it is for, so the
	 *  windows are announced first, in order, and the dock built once that is done. */
	#restore(id: WorkspaceId) {
		const api = this.#api;
		if (!api) return;
		const token = ++this.#restoreToken;
		const layout = layoutFor(settings.workspaces, id);
		const boxes = (layout.popoutGroups ?? []).map((p) => p.position);
		if (boxes.length === 0) {
			this.#restoreNow(id, layout);
			return;
		}
		// Nothing is saved while this is pending: the layout events are not the user's.
		this.#restoring = true;
		void popout.announce(boxes).then(() => {
			if (token !== this.#restoreToken || this.#api !== api) return;
			this.#restoreNow(id, layout);
		});
	}

	#restoreNow(id: WorkspaceId, layout: SerializedDockview) {
		const api = this.#api;
		if (!api) return;
		const withWindows = (layout.popoutGroups?.length ?? 0) > 0;
		this.#restoring = true;
		try {
			try {
				api.fromJSON(layout);
			} catch (e) {
				console.error('could not restore the layout', e);
				try {
					api.fromJSON(presetLayout(id));
				} catch (again) {
					// Neither took: say so rather than leave a dock that looks as if
					// nothing was asked of it.
					console.error('could not build the preset either', again);
					toast.error(`Could not arrange the ${workspaceSpec(id).label} workspace`, { description: String(again) });
				}
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
		if (!withWindows) {
			this.#takeReference();
			return;
		}
		// Its windows open from timers, after this returns, each as a layout change that
		// is no rearrangement. The baseline is the layout once they are all up.
		this.#reference = null;
		const settle = ++this.#settle;
		const token = this.#restoreToken;
		const up = Promise.race([api.popoutRestorationPromise, new Promise<void>((done) => setTimeout(done, POPOUT_RESTORE_MS))]);
		void up.then(() => {
			popout.settle();
			if (token === this.#restoreToken && settle === this.#settle && this.#api === api) this.#takeReference();
		});
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
			// A panel in a window of its own is brought to the front of the screen too.
			if (popout.isDetached(id)) popout.reveal(id);
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

	/** Put the active workspace back to its preset, forgetting what was saved: the
	 *  arrangement and the library tab picked in it. Says what it did — a workspace
	 *  that was never rearranged looks the same afterwards, and a reset that shows
	 *  nothing reads as one that did nothing. */
	reset() {
		const api = this.#api;
		if (!api) return;
		const id = this.active;
		const stored = id in settings.workspaces.layouts || id in settings.workspaces.library.tabs;
		// Nothing stored, and the dock is as restoring the preset left it.
		const asDefault = !stored && this.#reference !== null && sameArrangement(api.toJSON(), this.#reference);
		this.#abandonSave();
		// No entry, and none written back: what settles is the preset, which is
		// what the baseline will be.
		settings.resetWorkspace(id);
		this.#restore(id);
		const label = workspaceSpec(id).label;
		if (asDefault) toast.info(`${label} workspace is already in its default arrangement`);
		else toast.success(`${label} workspace reset to its default arrangement`);
	}

	/** Every workspace back to its preset; the one on screen is rebuilt. */
	resetAll() {
		if (!this.#api) return;
		this.#abandonSave();
		settings.resetAllWorkspaces();
		this.#restore(this.active);
		toast.success('All workspaces reset to their default arrangements');
	}

	/** Drop a change still waiting on the debounce: what it would write is being
	 *  thrown away. */
	#abandonSave() {
		if (!this.#timer) return;
		clearTimeout(this.#timer);
		this.#timer = null;
	}
}

export const workspace = new WorkspaceState();
