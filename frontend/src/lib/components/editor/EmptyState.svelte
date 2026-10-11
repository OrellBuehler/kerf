<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';

	let {
		icon,
		title,
		hint = undefined,
		pad = 36,
		children
	}: {
		icon: string;
		title: string;
		hint?: string;
		/** Vertical room above and below, px: a whole panel's worth, or less inside a list. */
		pad?: number;
		/** A way out — a button or two. */
		children?: Snippet;
	} = $props();
</script>

<!-- What a panel says when it has nothing to show: one icon, one line, one reason, one way out. -->
<div
	style="display:flex;flex-direction:column;align-items:center;gap:8px;padding:{pad}px 16px;text-align:center"
>
	<Icon n={icon} s={20} color="var(--text-muted)" />
	<div style="font-size:12px;font-weight:500;color:var(--text-secondary)">{title}</div>
	{#if hint}<div style="max-width:260px;font-size:12px;line-height:1.5;color:var(--text-muted)">{hint}</div>{/if}
	{#if children}<div style="margin-top:4px;display:flex;gap:6px;flex-wrap:wrap;justify-content:center">{@render children()}</div>{/if}
</div>
