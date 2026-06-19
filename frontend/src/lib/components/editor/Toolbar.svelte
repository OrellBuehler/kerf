<script lang="ts">
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import Btn from './Btn.svelte';
	import { ui, type Tool } from '$lib/editor-ui.svelte';

	let { onExport }: { onExport: () => void } = $props();

	const tools: [Tool, string, string][] = [
		['pointer', 'MousePointer2', 'Select (V)'],
		['razor', 'Scissors', 'Razor (C)'],
		['bookmark', 'Bookmark', 'Marker (M)']
	];
</script>

{#snippet divider()}
	<span style="width:1px;height:22px;background:var(--border-strong);margin:0 4px;flex:none"></span>
{/snippet}

<div
	style="height:var(--toolbar-h);display:flex;align-items:center;gap:6px;padding:0 12px;background:var(--surface-panel);border-bottom:1px solid var(--border-default);flex:none"
>
	{#each tools as [id, ic, t] (id)}
		<IconBtn title={t} active={ui.tool === id} onclick={() => (ui.tool = id)}>
			<Icon n={ic} />
		</IconBtn>
	{/each}
	<IconBtn title="Snap to clips" active={ui.snap} onclick={() => (ui.snap = !ui.snap)}>
		<Icon n="magnet" />
	</IconBtn>

	{@render divider()}

	<IconBtn title="Skip back"><Icon n="skip-back" /></IconBtn>
	<IconBtn
		title={ui.playing ? 'Pause' : 'Play'}
		onclick={() => (ui.playing = !ui.playing)}
		style="background:var(--surface-hover);color:var(--text-primary)"
	>
		<Icon n={ui.playing ? 'pause' : 'play'} />
	</IconBtn>
	<IconBtn title="Skip forward"><Icon n="skip-forward" /></IconBtn>
	<span style="font-family:var(--font-mono);font-size:13px;color:var(--kerf-300);margin-left:6px;font-weight:500">
		00:00:32:08
	</span>

	<div style="flex:1"></div>

	<Btn
		variant={ui.agentOpen ? 'agent' : 'ghost'}
		size="sm"
		icon="plug"
		onclick={() => (ui.agentOpen = !ui.agentOpen)}>Agent</Btn
	>
	{@render divider()}
	<Btn variant="primary" size="sm" icon="upload" onclick={onExport}>Export</Btn>
</div>
