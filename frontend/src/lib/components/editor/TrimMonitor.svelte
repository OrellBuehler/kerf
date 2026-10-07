<script lang="ts">
	/* What the Preview shows while a roll, slip or slide is being dragged: the frames
	 * either side of the edit as it would be left — a roll's outgoing last frame
	 * beside the incoming first, a slip's new in and out, a slide's two neighbours'
	 * changed edges — under the same readout the timeline draws beside the pointer.
	 * It replaces the playhead's composited frame for the length of the gesture (the
	 * playhead has not moved, and the picture there is not what is being decided) and
	 * is gone the moment the gesture ends, however it ends (`ui.trimMonitor` is null). */
	import TrimFrame from './TrimFrame.svelte';
	import type { TrimMonitor } from '$lib/trim-tools';

	let { monitor }: { monitor: TrimMonitor } = $props();

	const tone = $derived(
		monitor.tone === 'refused' ? 'var(--danger)' : monitor.tone === 'limit' ? 'var(--warning)' : 'var(--text-primary)'
	);
</script>

<div
	role="status"
	aria-live="off"
	data-trim-monitor
	style="position:absolute;inset:0;z-index:6;display:flex;flex-direction:column;gap:8px;padding:10px 12px;box-sizing:border-box;background:var(--surface-void);border-radius:inherit;pointer-events:none"
>
	<div style="flex:none;display:flex;flex-direction:column;gap:2px;min-width:0">
		<span
			style="font-family:var(--font-mono);font-size:12px;font-weight:600;color:{tone};white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
			>{monitor.title}</span
		>
		{#if monitor.detail}
			<span style="font-size:11px;color:{tone};opacity:.85;line-height:1.3">{monitor.detail}</span>
		{/if}
	</div>
	<div style="flex:1;min-height:0;display:flex;gap:8px">
		{#each monitor.cells as cell (cell.key)}
			<TrimFrame {cell} />
		{/each}
	</div>
</div>
