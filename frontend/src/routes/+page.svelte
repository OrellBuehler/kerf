<script lang="ts">
	import { onMount } from 'svelte';
	import * as Resizable from '$lib/components/ui/resizable';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { Film, Download } from '@lucide/svelte';
	import { toast } from 'svelte-sonner';
	import MediaBin from '$lib/components/MediaBin.svelte';
	import PreviewPlayer from '$lib/components/PreviewPlayer.svelte';
	import TimelineCanvas from '$lib/components/TimelineCanvas.svelte';
	import AgentPanel from '$lib/components/AgentPanel.svelte';
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

<div class="bg-background text-foreground flex h-screen flex-col">
	<header class="flex items-center justify-between border-b px-4 py-2">
		<div class="flex items-center gap-2">
			<div class="bg-primary text-primary-foreground flex size-7 items-center justify-center rounded">
				<Film class="size-4" />
			</div>
			<div>
				<div class="text-sm font-semibold leading-none">Kerf</div>
				<div class="text-muted-foreground text-xs">AI-assisted video editor</div>
			</div>
			{#if !inTauri()}
				<Badge variant="outline" class="ml-2">browser preview</Badge>
			{/if}
			{#if editor.error}
				<Badge variant="destructive" class="ml-2">{editor.error}</Badge>
			{/if}
		</div>
		<Button size="sm" onclick={onExport}>
			<Download class="size-4" />
			Export
		</Button>
	</header>

	<Resizable.PaneGroup direction="horizontal" class="flex-1">
		<Resizable.Pane defaultSize={20} minSize={14}>
			<MediaBin />
		</Resizable.Pane>
		<Resizable.Handle withHandle />
		<Resizable.Pane defaultSize={56}>
			<Resizable.PaneGroup direction="vertical">
				<Resizable.Pane defaultSize={58} minSize={25}>
					<PreviewPlayer />
				</Resizable.Pane>
				<Resizable.Handle withHandle />
				<Resizable.Pane defaultSize={42} minSize={20}>
					<TimelineCanvas />
				</Resizable.Pane>
			</Resizable.PaneGroup>
		</Resizable.Pane>
		<Resizable.Handle withHandle />
		<Resizable.Pane defaultSize={24} minSize={16}>
			<AgentPanel />
		</Resizable.Pane>
	</Resizable.PaneGroup>
</div>
