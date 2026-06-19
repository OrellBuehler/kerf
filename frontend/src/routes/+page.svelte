<script lang="ts">
	import { onMount } from 'svelte';
	import { toast } from 'svelte-sonner';
	import TitleBar from '$lib/components/editor/TitleBar.svelte';
	import Toolbar from '$lib/components/editor/Toolbar.svelte';
	import MediaBin from '$lib/components/editor/MediaBin.svelte';
	import Preview from '$lib/components/editor/Preview.svelte';
	import Timeline from '$lib/components/editor/Timeline.svelte';
	import AgentPanel from '$lib/components/editor/AgentPanel.svelte';
	import StatusBar from '$lib/components/editor/StatusBar.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { inTauri } from '$lib/api';

	onMount(() => {
		void editor.load();
	});

	function onExport() {
		if (!inTauri()) {
			toast.info('Export runs the in-process FFmpeg render in the desktop app.');
			return;
		}
		toast.info('Export wiring is stubbed — connect it to the export command.');
	}
</script>

<div style="position:fixed;inset:0;display:flex;flex-direction:column;background:var(--surface-void)">
	<TitleBar />
	<Toolbar {onExport} />
	<div style="flex:1;display:flex;min-height:0">
		<MediaBin />
		<div style="flex:1;display:flex;flex-direction:column;min-width:0">
			<Preview />
			<Timeline />
		</div>
		{#if ui.agentOpen}
			<AgentPanel />
		{/if}
	</div>
	<StatusBar />
</div>
