<script lang="ts">
	/* The overview strip under the toolbar: the whole cut with the view on it, and its header label. */
	import Minimap from '../Minimap.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import type { Target } from '#lib/minimap';
	import { editor } from '#lib/state.svelte';
	import type { Track } from '#lib/types';

	let {
		tracks,
		scrollX,
		viewW,
		pxPerSec,
		applyView
	}: {
		tracks: Track[];
		scrollX: number;
		viewW: number;
		pxPerSec: number;
		applyView: (target: Target) => void;
	} = $props();
</script>

<div
	aria-hidden="true"
	style="display:flex;flex:none;border-bottom:var(--line-width) solid var(--border-subtle);background:var(--surface-app)"
>
	<div
		style="width:var(--track-header-w);flex:none;box-sizing:border-box;border-right:var(--line-width) solid var(--border-default);display:flex;align-items:center;padding:0 8px;font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
	>
		Overview
	</div>
	<Minimap
		{tracks}
		duration={editor.duration}
		scrollLeft={scrollX}
		{viewW}
		{pxPerSec}
		time={ui.time}
		markIn={ui.markIn}
		markOut={ui.markOut}
		onview={applyView}
		onseek={(t) => ui.seek(t)}
	/>
</div>
