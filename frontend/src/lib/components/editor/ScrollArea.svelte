<script lang="ts">
	// A scrolling body with a cue for what is under its bottom edge: a fade into the panel's own surface and a
	// chevron, shown only while there is more to scroll to (`scroll-cue.ts`). The Deliver panel and the export
	// dialog pin their footers, so without it the end of the visible content looks like the end of the content.
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';
	import { moreBelow } from '#lib/scroll-cue';

	let { padding = '12px 16px', children }: { padding?: string; children: Snippet } = $props();

	let scroller = $state<HTMLDivElement | null>(null);
	let content = $state<HTMLDivElement | null>(null);
	let below = $state(false);

	const measure = () => {
		if (scroller) below = moreBelow(scroller);
	};

	// The content grows and shrinks as sections open and a verdict arrives, the area as the window is resized.
	$effect(() => {
		if (!scroller || !content) return;
		measure();
		const watch = new ResizeObserver(measure);
		watch.observe(scroller);
		watch.observe(content);
		return () => watch.disconnect();
	});
</script>

<div style="position:relative;flex:1;min-height:0;display:flex;flex-direction:column">
	<div bind:this={scroller} onscroll={measure} style="flex:1;min-height:0;overflow-y:auto;padding:{padding}">
		<div bind:this={content}>{@render children()}</div>
	</div>
	<div
		aria-hidden="true"
		data-scroll-cue={below ? 'more' : undefined}
		style="position:absolute;left:0;right:0;bottom:0;height:36px;display:flex;align-items:flex-end;justify-content:center;padding-bottom:2px;pointer-events:none;background:linear-gradient(to top, var(--surface-panel) 15%, transparent);opacity:{below
			? 1
			: 0};transition:opacity var(--dur-fast) var(--ease-out)"
	>
		<Icon n="chevron-down" s={14} color="var(--text-muted)" />
	</div>
</div>
