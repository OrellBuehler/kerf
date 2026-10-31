<script lang="ts">
	// A sound effect's shape over its length, drawn like a waveform from `sfx-panel.ts`'s envelope of its kind — nothing is
	// rendered, so a tile costs a path. The tick is where the hit lands on the picture event; `reverse` plays it backwards.
	import { sketchOf } from '#lib/sfx-panel';
	import type { SfxPreset } from '#lib/types';

	let {
		preset,
		hue,
		reverse = false,
		height = 16,
		tick = true,
		label = undefined
	}: {
		preset: Pick<SfxPreset, 'kind' | 'anchor' | 'default_duration_s'>;
		/** A theme colour (`var(--agent-400)`): the shape is that at a quarter strength with the outline in full. */
		hue: string;
		reverse?: boolean;
		height?: number;
		/** Draw the hit point. */
		tick?: boolean;
		/** The accessible name, when the sketch says something a neighbour does not; otherwise it is decoration. */
		label?: string;
	} = $props();

	const W = 120;
	const sketch = $derived(sketchOf(preset, W, 24, reverse));
</script>

<svg
	viewBox="0 0 {W} 24"
	preserveAspectRatio="none"
	role={label ? 'img' : undefined}
	aria-label={label}
	aria-hidden={label ? undefined : 'true'}
	style="display:block;width:100%;height:{height}px;overflow:visible;color:{hue}"
>
	<path
		d={sketch.d}
		fill="color-mix(in srgb, currentColor 30%, transparent)"
		stroke="currentColor"
		stroke-width="1.1"
		stroke-linejoin="round"
		vector-effect="non-scaling-stroke"
	/>
	{#if tick}
		<line
			x1={sketch.hit * W}
			x2={sketch.hit * W}
			y1="-2"
			y2="26"
			stroke="var(--text-primary)"
			stroke-width="1"
			stroke-dasharray="2 2"
			vector-effect="non-scaling-stroke"
			opacity="0.7"
		/>
	{/if}
</svg>
