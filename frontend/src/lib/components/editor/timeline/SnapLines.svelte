<script lang="ts">
	/* The line through the lanes where a held edge is on a magnet, and the magnet's name. */
	import type { SnapLine } from '#lib/snap';

	let {
		snapNow,
		pxPerSec,
		scrollX,
		viewW
	}: {
		snapNow: SnapLine[];
		pxPerSec: number;
		scrollX: number;
		viewW: number;
	} = $props();
</script>

{#each snapNow as line (line.at)}
	<div
		data-snap-line
		aria-hidden="true"
		style="position:absolute;left:{line.at * pxPerSec - 0.5}px;top:var(--ruler-h);bottom:0;width:var(--line-width);z-index:34;pointer-events:none;background:color-mix(in srgb,var(--text-primary) 85%,transparent)"
	>
		<span
			data-snap-label
			style="position:sticky;top:calc(var(--ruler-h) + 3px);display:block;width:max-content;margin-left:{line.at * pxPerSec - scrollX + 96 > viewW ? '-96px' : '4px'};padding:1px 5px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--border-strong);background:var(--surface-raised);color:var(--text-primary);font-family:var(--font-mono);font-size:10px;line-height:13px;white-space:nowrap"
			>{line.label}</span
		>
	</div>
{/each}
