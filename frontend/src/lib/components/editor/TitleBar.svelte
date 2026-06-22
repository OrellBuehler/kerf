<script lang="ts">
	import KerfMark from './KerfMark.svelte';
	import Badge from './Badge.svelte';
	import { editor } from '$lib/state.svelte';
</script>

<div
	style="height:var(--titlebar-h);display:flex;align-items:center;gap:10px;padding:0 12px;background:var(--surface-app);border-bottom:1px solid var(--border-default);flex:none;-webkit-app-region:drag"
>
	<div style="flex:1;text-align:center;display:flex;align-items:center;justify-content:center;gap:8px">
		<KerfMark size={15} />
		<span style="font:var(--type-label);color:var(--text-secondary)">{editor.projectName}</span>
		{#if editor.saved}
			<Badge tone="success" dot>Saved</Badge>
		{:else}
			<Badge tone="warning" dot>Unsaved</Badge>
		{/if}
	</div>
	<span
		title={editor.currentPath ?? 'In-memory project — not yet saved'}
		style="font-family:var(--font-mono);font-size:11px;color:var(--text-disabled);max-width:280px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
		>{editor.currentPath ?? 'local · in-memory'}</span
	>
</div>
