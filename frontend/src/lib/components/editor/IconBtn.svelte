<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		active = undefined,
		title = undefined,
		size = 32,
		style = '',
		class: klass = '',
		children,
		...rest
	}: {
		active?: boolean;
		title?: string;
		size?: number;
		style?: string;
		class?: string;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<!-- `aria-pressed` only when it is a toggle (`active` given): a plain icon button announced as
     "not pressed" reads as a switch. Colors and states are `styles/controls.css`. -->
<button
	class={['k-ibtn', klass]}
	{title}
	aria-label={title}
	aria-pressed={active}
	style="width:{size}px;height:{size}px;{style}"
	{...rest}
>
	{@render children?.()}
</button>
