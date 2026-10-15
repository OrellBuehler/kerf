<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';
	import { tip, type TipInput } from '#lib/tip';

	let {
		variant = 'secondary',
		size = 'md',
		icon = undefined,
		iconSize = undefined,
		style = '',
		class: klass = '',
		title = undefined,
		children,
		...rest
	}: {
		variant?: 'primary' | 'agent' | 'secondary' | 'ghost' | 'destructive';
		size?: 'sm' | 'md' | 'lg';
		icon?: string;
		iconSize?: number;
		style?: string;
		class?: string;
		title?: TipInput;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<!-- Colors, hover, pressed and disabled live in `styles/controls.css`: an inline background
     could not be hovered. `style` is for layout only. -->
<button class={['k-btn', klass]} use:tip={title} data-variant={variant} data-size={size} style={style || undefined} {...rest}>
	{#if icon}<Icon n={icon} s={iconSize ?? 14} />{/if}
	{@render children?.()}
</button>
