<script lang="ts">
	// The editor's movable middle: dockview hosts one Svelte component per
	// panel. Which arrangement it opens in, switching between them and saving
	// them are `workspace`'s job (the title bar's tabs drive it); this builds the
	// dock and mounts the panels. TitleBar (with the menu bar) and StatusBar stay fixed
	// around it.
	import { onMount, mount, unmount, type Component } from 'svelte';
	import { createDockview, type CreateComponentOptions, type IContentRenderer } from 'dockview';
	import LibraryPanel from './LibraryPanel.svelte';
	import Preview from './Preview.svelte';
	import Timeline from './Timeline.svelte';
	import Inspector from './Inspector.svelte';
	import AgentPanel from './AgentPanel.svelte';
	import DeliverPanel from './DeliverPanel.svelte';
	import Mixer from './Mixer.svelte';
	import ContextMenu from './ContextMenu.svelte';
	import { POPOUT_URL, isPanelId, type PanelId } from '$lib/layout';
	import { workspace } from '$lib/workspace.svelte';
	import { popout } from '$lib/popout.svelte';

	const COMPONENTS: Record<PanelId, Component<any>> = {
		library: LibraryPanel,
		preview: Preview,
		timeline: Timeline,
		inspector: Inspector,
		agent: AgentPanel,
		deliver: DeliverPanel,
		mixer: Mixer
	};

	let el = $state<HTMLDivElement | null>(null);

	/** A detached window needs a context menu of its own: it is drawn in the document of
	 *  the window that was clicked, and a Svelte root registers its delegated event
	 *  handlers on its own container, so the menu is mounted in the window's body rather
	 *  than reached from the editor's. */
	function menuIn(win: Window): () => void {
		const host = win.document.createElement('div');
		win.document.body.appendChild(host);
		const menu = mount(ContextMenu, { target: host, props: { win } });
		return () => {
			void unmount(menu);
			host.remove();
		};
	}

	function createComponent(o: CreateComponentOptions): IContentRenderer {
		const element = document.createElement('div');
		element.style.cssText = 'width:100%;height:100%;display:flex;flex-direction:column;overflow:hidden';
		let instance: Record<string, unknown> | null = null;
		return {
			element,
			init(params) {
				const C = COMPONENTS[o.name as PanelId];
				// The library sizes its own group (folded, it is just its rail) and
				// hands the space on to its neighbour, so it is given the panel it
				// lives in and the dock around it.
				if (C)
					instance = mount(C, {
						target: element,
						props: o.name === 'library' ? { panelApi: params.api, dock: params.containerApi } : {}
					});
			},
			dispose() {
				if (instance) void unmount(instance);
				instance = null;
			}
		};
	}

	function createWatermarkComponent() {
		const element = document.createElement('div');
		element.style.cssText =
			'display:grid;place-items:center;height:100%;font-size:12px;color:var(--text-disabled)';
		element.textContent = 'Open a panel from the Window menu';
		return { element, init() {} };
	}

	onMount(() => {
		const api = createDockview(el!, {
			createComponent,
			createWatermarkComponent,
			theme: { name: 'kerf', className: 'dockview-theme-kerf' },
			disableFloatingGroups: true,
			// What a panel moved into a window of its own opens at (`static/popout.html`).
			popoutUrl: POPOUT_URL,
			// The tab's right-click menu: the way to a window of its own and back.
			getTabContextMenuItems: ({ panel }) => {
				const id = panel.id;
				if (!isPanelId(id)) return [];
				if (popout.isDetached(id)) {
					return [{ label: 'Return to the editor window', action: () => popout.dockBack(id) }, 'separator', 'close'];
				}
				return [
					{ label: 'Move to new window', disabled: popout.blocked(id) !== null, action: () => void popout.popOut(id) },
					'separator',
					'close'
				];
			}
		});
		// Before the workspace is restored: a saved layout may hold windows to open.
		popout.attach(api, { window: menuIn });
		workspace.attach(api, el!);
		return () => {
			workspace.detach();
			popout.detach();
			api.dispose();
		};
	});
</script>

<div bind:this={el} style="flex:1;min-height:0;min-width:0;position:relative"></div>
