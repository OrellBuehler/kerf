<script lang="ts">
	import { ui } from '$lib/editor-ui.svelte';
	import { PHASES } from './data';
</script>

<div
	style="height:var(--statusbar-h);flex:none;display:flex;align-items:center;gap:12px;padding:0 12px;background:var(--surface-app);border-top:1px solid var(--border-default)"
>
	<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">
		23.976 fps · 3840×2160 · ProRes
	</span>
	<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">
		{ui.phase === 'editing' ? '02:24:00' : '04:12:00'}
	</span>
	<div style="flex:1"></div>
	<span style="font-size:10px;color:var(--text-disabled)">Demo state</span>
	<div
		style="display:flex;gap:2px;background:var(--surface-inset);border:1px solid var(--border-strong);border-radius:var(--radius-sm);padding:2px"
	>
		{#each PHASES as [id, label] (id)}
			<button
				onclick={() => ui.setPhase(id)}
				style="padding:3px 9px;border-radius:3px;border:none;cursor:pointer;font-family:var(--font-sans);font-size:11px;font-weight:500;background:{ui.phase ===
				id
					? 'var(--kerf-500)'
					: 'transparent'};color:{ui.phase === id ? 'var(--text-on-accent)' : 'var(--text-muted)'}"
			>
				{label}
			</button>
		{/each}
	</div>
</div>
