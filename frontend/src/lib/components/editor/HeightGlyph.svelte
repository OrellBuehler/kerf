<script lang="ts">
	/* A track-height preset drawn as what it does: the same box cut into more,
	 * thinner rows (compact), the usual three (medium), or two fat ones (large). */
	import type { HeightPreset } from '$lib/track-heights';

	let { preset, s = 14 }: { preset: HeightPreset; s?: number } = $props();

	/** Row height and count inside a 12 px box, with a 1 px gap between rows. */
	const rows = $derived(preset === 'compact' ? 4 : preset === 'medium' ? 3 : 2);
	const rowH = $derived((12 - (rows - 1)) / rows);
</script>

<svg width={s} height={s} viewBox="0 0 14 14" aria-hidden="true" style="display:block;flex:none">
	{#each Array.from({ length: rows }, (_, i) => i) as i (i)}
		<rect x="1" y={1 + i * (rowH + 1)} width="12" height={rowH} rx="0.8" fill="currentColor" />
	{/each}
</svg>
