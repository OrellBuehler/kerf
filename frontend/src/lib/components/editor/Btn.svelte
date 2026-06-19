<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';

	let {
		variant = 'secondary',
		size = 'md',
		icon = undefined,
		iconSize = undefined,
		style = '',
		children,
		...rest
	}: {
		variant?: 'primary' | 'agent' | 'secondary' | 'ghost' | 'destructive';
		size?: 'sm' | 'md' | 'lg';
		icon?: string;
		iconSize?: number;
		style?: string;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();

	const sizes = {
		sm: { h: 26, p: '0 10px', f: 12 },
		md: { h: 32, p: '0 14px', f: 13 },
		lg: { h: 38, p: '0 18px', f: 14 }
	} as const;

	const variants = {
		primary: 'background:var(--kerf-500);color:var(--text-on-accent);border:1px solid var(--kerf-500);',
		agent: 'background:var(--agent-500);color:var(--agent-fg);border:1px solid var(--agent-500);',
		secondary:
			'background:var(--surface-hover);color:var(--text-primary);border:1px solid var(--border-strong);',
		ghost: 'background:transparent;color:var(--text-secondary);border:1px solid transparent;',
		destructive: 'background:transparent;color:var(--red-400);border:1px solid var(--red-600);'
	} as const;

	const sz = $derived(sizes[size]);
</script>

<button
	style="display:inline-flex;align-items:center;justify-content:center;gap:7px;height:{sz.h}px;padding:{sz.p};font-family:var(--font-sans);font-size:{sz.f}px;font-weight:500;line-height:1;border-radius:var(--radius-sm);cursor:pointer;white-space:nowrap;transition:background var(--dur-fast) var(--ease-out);{variants[
		variant
	]}{style}"
	{...rest}
>
	{#if icon}<Icon n={icon} s={iconSize ?? 14} />{/if}
	{@render children?.()}
</button>
