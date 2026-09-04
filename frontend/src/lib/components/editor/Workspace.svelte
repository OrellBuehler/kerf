<script lang="ts">
	// The editor's movable middle: dockview hosts one Svelte component per
	// panel, and the arrangement is saved through the app settings so it
	// survives a restart. TitleBar, Toolbar and StatusBar stay fixed around it.
	import { onMount, mount, unmount, type Component } from 'svelte';
	import { createDockview, type CreateComponentOptions, type IContentRenderer } from 'dockview';
	import MediaBin from './MediaBin.svelte';
	import TranscriptPanel from './TranscriptPanel.svelte';
	import Preview from './Preview.svelte';
	import Timeline from './Timeline.svelte';
	import Inspector from './Inspector.svelte';
	import AgentPanel from './AgentPanel.svelte';
	import { DEFAULT_LAYOUT, sanitizeLayout, type PanelId } from '$lib/layout';
	import { workspace } from '$lib/workspace.svelte';
	import { settings } from '$lib/settings.svelte';

	const COMPONENTS: Record<PanelId, Component> = {
		media: MediaBin,
		transcript: TranscriptPanel,
		preview: Preview,
		timeline: Timeline,
		inspector: Inspector,
		agent: AgentPanel
	};

	let el = $state<HTMLDivElement | null>(null);

	function createComponent(o: CreateComponentOptions): IContentRenderer {
		const element = document.createElement('div');
		element.style.cssText = 'width:100%;height:100%;display:flex;flex-direction:column;overflow:hidden';
		let instance: Record<string, unknown> | null = null;
		return {
			element,
			init() {
				const C = COMPONENTS[o.name as PanelId];
				if (C) instance = mount(C, { target: element });
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
		element.textContent = 'Open a panel from the toolbar';
		return { element, init() {} };
	}

	onMount(() => {
		const api = createDockview(el!, {
			createComponent,
			createWatermarkComponent,
			theme: { name: 'kerf', className: 'dockview-theme-kerf' },
			disableFloatingGroups: true
		});
		let restoring = true;
		try {
			api.fromJSON(sanitizeLayout(settings.layout) ?? structuredClone(DEFAULT_LAYOUT));
		} catch (e) {
			console.error('could not restore the layout', e);
			api.fromJSON(structuredClone(DEFAULT_LAYOUT));
		}
		restoring = false;
		workspace.attach(api);

		let timer: ReturnType<typeof setTimeout> | null = null;
		const save = () => {
			timer = null;
			void settings.setLayout(api.toJSON());
		};
		const sub = api.onDidLayoutChange(() => {
			if (restoring) return;
			if (timer) clearTimeout(timer);
			timer = setTimeout(save, 500);
		});
		return () => {
			if (timer) {
				clearTimeout(timer);
				save();
			}
			sub.dispose();
			workspace.detach();
			api.dispose();
		};
	});
</script>

<div bind:this={el} style="flex:1;min-height:0;min-width:0;position:relative"></div>
