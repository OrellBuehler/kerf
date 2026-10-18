<script lang="ts">
	import type { Snippet } from 'svelte';
	import { tip, type TipInput } from '$lib/tip';

	let {
		title = undefined,
		class: klass = '',
		pending = false,
		onclick = undefined,
		children,
		...rest
	}: {
		title?: TipInput;
		class?: string;
		/** An edit is in flight: off to the eye and the click, but still focusable (see `Btn`). */
		pending?: boolean;
		onclick?: unknown;
		children?: Snippet;
		[key: string]: unknown;
	} = $props();
</script>

<!-- A preset in a grid of them: one line, ending in an ellipsis rather than running into its border.
     See `.k-tile` in `styles/controls.css`. -->
<button
	class={['k-tile', klass]}
	use:tip={title}
	aria-disabled={pending ? 'true' : undefined}
	onclick={(e: MouseEvent) => {
		if (pending) return;
		(onclick as ((e: MouseEvent) => void) | undefined)?.(e);
	}}
	{...rest}
>
	{@render children?.()}
</button>
