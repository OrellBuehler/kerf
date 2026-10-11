<script lang="ts">
	import type { Snippet } from 'svelte';
	import { tip, type TipInput } from '$lib/tip';

	let {
		title = undefined,
		class: klass = '',
		children,
		...rest
	}: {
		title?: TipInput;
		class?: string;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<!-- A preset in a grid of them: one line, ending in an ellipsis rather than running into its border.
     See `.k-tile` in `styles/controls.css`. -->
<button class={['k-tile', klass]} use:tip={title} {...rest}>
	{@render children?.()}
</button>
