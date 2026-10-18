<script lang="ts">
	// The render bar: a thin strip along the top of the ruler saying how the Preview will play each
	// span (`render-bar.ts`, `get_render_bar`): green where the GPU preview draws it, yellow where it
	// hands over to the FFmpeg stream, red where playback stalls. With the GPU preview off only the
	// red spans show. Asked again a moment after the cut, the media or the setting changes.
	import { untrack } from 'svelte';
	import { editor } from '$lib/state.svelte';
	import { settings } from '$lib/settings.svelte';
	import { getRenderBar } from '$lib/api';
	import { tip } from '$lib/tip';
	import { barColor, barTip, spansIn } from '$lib/render-bar';
	import type { BarSpan } from '$lib/types';

	let {
		pxPerSec,
		from,
		to,
		tc
	}: { pxPerSec: number; from: number; to: number; tc: (t: number) => string } = $props();

	let spans = $state<BarSpan[]>([]);
	let timer: ReturnType<typeof setTimeout> | null = null;
	let asked = 0;

	$effect(() => {
		void editor.timeline;
		void editor.assets;
		void settings.gpuPreview;
		untrack(() => {
			if (timer) clearTimeout(timer);
			timer = setTimeout(() => {
				timer = null;
				const n = ++asked;
				getRenderBar()
					.then((bar) => {
						if (n === asked) spans = bar.spans;
					})
					.catch(() => {
						if (n === asked) spans = [];
					});
			}, 400);
		});
		return () => {
			if (timer) clearTimeout(timer);
		};
	});

	const shown = $derived(spansIn(spans, from, to));
</script>

{#each shown as s (s.start)}
	<span
		data-render-bar={s.state}
		use:tip={barTip(s, tc)}
		style="position:absolute;left:{s.start * pxPerSec}px;width:{Math.max(
			1,
			(s.end - s.start) * pxPerSec
		)}px;top:0;height:3px;background:{barColor(s.state)};opacity:.85"
	></span>
{/each}
