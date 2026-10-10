<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';

	let {
		variant = 'secondary',
		size = 'md',
		icon = undefined,
		iconSize = undefined,
		style = '',
		class: klass = '',
		children,
		...rest
	}: {
		variant?: 'primary' | 'agent' | 'secondary' | 'ghost' | 'destructive';
		size?: 'sm' | 'md' | 'lg';
		icon?: string;
		iconSize?: number;
		style?: string;
		class?: string;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<!-- Colors, hover, pressed and disabled live in `styles/controls.css`: an inline background
     could not be hovered. `style` is for layout only. -->
<button class={['k-btn', klass]} data-variant={variant} data-size={size} style={style || undefined} {...rest}>
	{#if icon}<Icon n={icon} s={iconSize ?? 14} />{/if}
	{@render children?.()}
</button>
