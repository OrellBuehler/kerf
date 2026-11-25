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
	import SoundFx from './SoundFx.svelte';
	import Scopes from './Scopes.svelte';
	import DopeSheet from './DopeSheet.svelte';
	import ProxyQueue from './ProxyQueue.svelte';
	import DockEmpty from './DockEmpty.svelte';
	import ContextMenu from './ContextMenu.svelte';
	import { POPOUT_URL, isPanelId, type PanelId } from '#lib/layout';
	import { workspace } from '#lib/workspace.svelte';
	import { popout } from '#lib/popout.svelte';
	import { abandonDragOnEscape, onlyPrimaryDrags } from '#lib/dock-drag';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { agentTabLabel, proposalIndicator } from '#lib/agent-status';

	const COMPONENTS: Record<PanelId, Component<any>> = {
		library: LibraryPanel,
		preview: Preview,
		timeline: Timeline,
		inspector: Inspector,
		agent: AgentPanel,
		deliver: DeliverPanel,
		mixer: Mixer,
		sfx: SoundFx,
		scopes: Scopes,
		keyframes: DopeSheet,
		proxyQueue: ProxyQueue
	};

	let el = $state<HTMLDivElement | null>(null);

	// A proposal the agent staged shows on the Agent tab: in the Audio and Deliver workspaces the agent is only a
	// tab, and a proposal that landed there was invisible. The tab is dockview's own element, so the count is a
	// data attribute the stylesheet draws (`dockview-kerf.css`) and the tab's name says what it holds.
	const pending = $derived(proposalIndicator(editor.staged));
	$effect(() => {
		const host = el;
		const badge = pending;
		// The panels are rebuilt on a workspace switch and on opening one: look again when they change.
		void workspace.open;
		void workspace.active;
		if (!host) return;
		const apply = () => {
			for (const tab of host.querySelectorAll<HTMLElement>('.dv-tab[data-tab-panel-id="agent"]')) {
				const label = tab.querySelector<HTMLElement>('.dv-default-tab-content');
				if (badge) {
					label?.setAttribute('data-pending', badge.count > 0 ? String(badge.count) : '•');
					tab.setAttribute('aria-label', agentTabLabel(badge));
				} else {
					label?.removeAttribute('data-pending');
					tab.setAttribute('aria-label', agentTabLabel(null));
				}
			}
		};
		apply();
		const again = requestAnimationFrame(apply);
		return () => cancelAnimationFrame(again);
	});

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
				// lives in and the dock around it. The preview is told when it is behind
				// another tab, so it makes no frames for a picture nobody sees.
				if (C)
					instance = mount(C, {
						target: element,
						props:
							o.name === 'library'
								? { panelApi: params.api, dock: params.containerApi }
								: o.name === 'preview'
									? { panelApi: params.api }
									: {}
					});
			},
			dispose() {
				if (instance) void unmount(instance);
				instance = null;
			}
		};
	}

	/** What the dock shows with every panel closed (`DockEmpty`): how to get one back. */
	function createWatermarkComponent() {
		const element = document.createElement('div');
		element.style.cssText = 'height:100%';
		let instance: Record<string, unknown> | null = null;
		return {
			element,
			init() {
				instance = mount(DockEmpty, { target: element });
			},
			dispose() {
				if (instance) void unmount(instance);
				instance = null;
			}
		};
	}

	onMount(() => {
		const api = createDockview(el!, {
			createComponent,
			createWatermarkComponent,
			theme: { name: 'kerf', className: 'dockview-theme-kerf' },
			disableFloatingGroups: true,
			// A panel is dragged with pointer events: the app's drag-drop handler (kept on for dropping files) leaves
			// the webview no in-page HTML5 drag on Windows. Escape and a lost window are `abandonDragOnEscape`'s.
			dndStrategy: 'pointer',
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
		// A drag is given up on Escape, a lost window, a window opening or closing and before the dock is rebuilt; and only the
		// primary button starts one, here and in every window a panel is moved to.
		const dragging = abandonDragOnEscape(api);
		const primary = onlyPrimaryDrags(window);
		// Before the workspace is restored: a saved layout may hold windows to open.
		popout.attach(api, {
			window: (win) => {
				const menu = menuIn(win);
				const only = onlyPrimaryDrags(win);
				return () => {
					only();
					menu();
				};
			}
		});
		workspace.attach(api, el!, { beforeRebuild: () => dragging.abandon() });
		ui.focusedPanel = api.activePanel?.id ?? null;
		const active = api.onDidActivePanelChange((e) => (ui.focusedPanel = e.panel?.id ?? null));
		return () => {
			active.dispose();
			dragging.dispose();
			primary();
			ui.focusedPanel = null;
			workspace.detach();
			popout.detach();
			api.dispose();
		};
	});
</script>

<div bind:this={el} style="flex:1;min-height:0;min-width:0;position:relative"></div>
