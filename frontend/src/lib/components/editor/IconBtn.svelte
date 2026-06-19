<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		active = false,
		title = undefined,
		size = 30,
		style = '',
		children,
		...rest
	}: {
		active?: boolean;
		title?: string;
		size?: number;
		style?: string;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<button
	{title}
	aria-pressed={active}
	style="display:inline-flex;align-items:center;justify-content:center;width:{size}px;height:{size}px;border-radius:var(--radius-sm);cursor:pointer;border:1px solid {active
		? 'var(--border-strong)'
		: 'transparent'};background:{active
		? 'var(--surface-active)'
		: 'transparent'};color:{active
		? 'var(--kerf-400)'
		: 'var(--text-secondary)'};transition:background var(--dur-fast) var(--ease-out),color var(--dur-fast) var(--ease-out);{style}"
	{...rest}
>
	{@render children?.()}
</button>
