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
	import type { PanelId } from '$lib/layout';
	import { workspace } from '$lib/workspace.svelte';

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
			disableFloatingGroups: true
		});
		workspace.attach(api, el!);
		return () => {
			workspace.detach();
			api.dispose();
		};
	});
</script>

<div bind:this={el} style="flex:1;min-height:0;min-width:0;position:relative"></div>
